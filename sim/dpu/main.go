// sim-dpu simulates the DPU side of DPF on OpenShift for DPUs provisioned by
// mock-dms. For every DPU past OS install it reads the ignition DPF generated
// (the DPF HCP provisioner's bf.cfg), joins the hosted cluster the way the
// DPU's kubelet would (CSRs approved by the provisioner), and stands in for
// sfc-controller by marking that node's ServiceInterfaces and ServiceChains
// Ready.
package main

import (
	"context"
	"flag"
	"log/slog"
	"os"
	"os/signal"
	"syscall"
	"time"

	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/labels"
	"k8s.io/apimachinery/pkg/runtime/schema"
	"k8s.io/client-go/dynamic"
	"k8s.io/client-go/rest"
	"k8s.io/client-go/tools/clientcmd"
)

const fakeNodeLabel = "node-role.dpf.nvidia.com/fake"

var dpuGVR = schema.GroupVersionResource{Group: "provisioning.dpu.nvidia.com", Version: "v1alpha1", Resource: "dpus"}

// Phases in which a real DPU has booted its installed OS and its kubelet is
// trying to join.
var joiningPhases = map[string]bool{
	"DPU Config":                 true,
	"Host Network Configuration": true,
	"DPU Cluster Config":         true,
	"Service Readiness":          true,
	"Node Effect Removal":        true,
	"Ready":                      true,
}

type options struct {
	namespace        string
	bfbRegistry      string
	hostedKubeconfig string
	nodeIP           string
	numSFs           int
	interval         time.Duration
	hostSelector     labels.Selector
}

func main() {
	opts := &options{}
	flag.StringVar(&opts.namespace, "namespace", "dpf-operator-system", "Namespace of the DPU objects.")
	flag.StringVar(&opts.bfbRegistry, "bfb-registry", "http://bfb-registry.dpf-operator-system.svc:8082", "bfb-registry URL to download bf.cfg from.")
	flag.StringVar(&opts.hostedKubeconfig, "hosted-kubeconfig", "", "Admin kubeconfig of the hosted cluster, used only for the sfc-controller stand-in.")
	flag.StringVar(&opts.nodeIP, "node-ip", os.Getenv("HOST_IP"), "InternalIP reported by the simulated DPU Nodes.")
	flag.IntVar(&opts.numSFs, "num-sfs", 64, "nvidia.com/bf_sf advertised by each simulated DPU Node.")
	flag.DurationVar(&opts.interval, "interval", 10*time.Second, "Reconcile interval.")
	hostSelector := flag.String("host-selector", "dpf.openshift.io/sim-level=m1", "Label selector on host Nodes whose DPUs sim-dpu joins; mock-dms must skip the same hosts.")
	flag.Parse()
	var err error
	if opts.hostSelector, err = labels.Parse(*hostSelector); err != nil {
		fatal("--host-selector", err)
	}

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()

	mgmtCfg, err := rest.InClusterConfig()
	if err != nil {
		mgmtCfg, err = clientcmd.NewNonInteractiveDeferredLoadingClientConfig(
			clientcmd.NewDefaultClientConfigLoadingRules(), nil).ClientConfig()
	}
	if err != nil {
		fatal("management cluster config", err)
	}
	mgmt, err := dynamic.NewForConfig(mgmtCfg)
	if err != nil {
		fatal("management cluster client", err)
	}
	var hosted dynamic.Interface
	if opts.hostedKubeconfig != "" {
		cfg, err := clientcmd.BuildConfigFromFlags("", opts.hostedKubeconfig)
		if err != nil {
			fatal("hosted cluster config", err)
		}
		if hosted, err = dynamic.NewForConfig(cfg); err != nil {
			fatal("hosted cluster client", err)
		}
	}

	s := &simulator{opts: opts, mgmt: mgmt, hosted: hosted, joins: map[string]*kubeletJoin{}}
	ticker := time.NewTicker(opts.interval)
	defer ticker.Stop()
	for {
		s.reconcile(ctx)
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
	}
}

type simulator struct {
	opts   *options
	mgmt   dynamic.Interface
	hosted dynamic.Interface
	// joins is keyed by DPU UID, so a reprovisioned DPU joins again.
	joins map[string]*kubeletJoin
}

func (s *simulator) reconcile(ctx context.Context) {
	dpus, err := s.mgmt.Resource(dpuGVR).Namespace(s.opts.namespace).List(ctx, metav1ListOptions())
	if err != nil {
		slog.Error("list DPUs", "err", err)
		return
	}
	seen := map[string]bool{}
	for i := range dpus.Items {
		dpu := &dpus.Items[i]
		uid := string(dpu.GetUID())
		seen[uid] = true
		phase, _, _ := unstructured.NestedString(dpu.Object, "status", "phase")
		if !joiningPhases[phase] || !s.hostSelected(ctx, dpu) {
			continue
		}
		s.reconcileJoin(ctx, dpu, phase)
	}
	for uid := range s.joins {
		if !seen[uid] {
			delete(s.joins, uid)
		}
	}
	if s.hosted != nil {
		s.reconcileSFC(ctx)
	}
}

func (s *simulator) reconcileJoin(ctx context.Context, dpu *unstructured.Unstructured, phase string) {
	log := slog.With("dpu", dpu.GetName(), "phase", phase)
	uid := string(dpu.GetUID())
	j := s.joins[uid]
	if j == nil {
		bfcfg, _, _ := unstructured.NestedString(dpu.Object, "status", "bfCFGFile")
		if bfcfg == "" {
			log.Info("waiting for status.bfCFGFile")
			return
		}
		ign, err := fetchDPUIgnition(ctx, s.opts.bfbRegistry, bfcfg)
		if err != nil {
			log.Error("read DPU ignition", "bfcfg", bfcfg, "err", err)
			return
		}
		if j, err = newKubeletJoin(ign); err != nil {
			log.Error("prepare kubelet join", "err", err)
			return
		}
		s.joins[uid] = j
		log.Info("read ignition from bf.cfg", "hostname", j.hostname, "apiserver", j.bootstrap.Host)
	}
	if j.done() {
		return
	}
	msg, err := j.step(ctx, s.opts)
	if err != nil {
		log.Error("kubelet join", "hostname", j.hostname, "err", err)
		return
	}
	if msg != "" {
		log.Info(msg, "hostname", j.hostname)
	}
}

// hostSelected reports whether the DPU's host Node is one sim-dpu joins.
func (s *simulator) hostSelected(ctx context.Context, dpu *unstructured.Unstructured) bool {
	host, _, _ := unstructured.NestedString(dpu.Object, "spec", "dpuNodeName")
	node, err := s.mgmt.Resource(schema.GroupVersionResource{Version: "v1", Resource: "nodes"}).Get(ctx, host, metav1.GetOptions{})
	if err != nil {
		slog.Error("get host Node", "dpu", dpu.GetName(), "host", host, "err", err)
		return false
	}
	return s.opts.hostSelector.Matches(labels.Set(node.GetLabels()))
}

func fatal(what string, err error) {
	slog.Error(what, "err", err)
	os.Exit(1)
}
