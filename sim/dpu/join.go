package main

import (
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/pem"
	"fmt"
	"net"

	certificatesv1 "k8s.io/api/certificates/v1"
	corev1 "k8s.io/api/core/v1"
	apierrors "k8s.io/apimachinery/pkg/api/errors"
	"k8s.io/apimachinery/pkg/api/resource"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes"
	"k8s.io/client-go/rest"
	"k8s.io/client-go/tools/clientcmd"
	"k8s.io/client-go/util/retry"
)

// kubeletJoin replays what the kubelet of an RHCOS DPU does on first boot:
// TLS bootstrap with the node-bootstrapper credentials from the ignition,
// register the Node with its own client certificate, then request a serving
// certificate. Both CSRs must be approved by the DPF HCP provisioner.
type kubeletJoin struct {
	hostname  string
	bootstrap *rest.Config

	clientKey  *ecdsa.PrivateKey
	clientCSR  string
	clientCert []byte

	nodeRegistered bool

	servingKey  *ecdsa.PrivateKey
	servingCSR  string
	servingCert []byte
}

func newKubeletJoin(ign *dpuIgnition) (*kubeletJoin, error) {
	cfg, err := clientcmd.RESTConfigFromKubeConfig(ign.BootstrapKubeconfig)
	if err != nil {
		return nil, fmt.Errorf("bootstrap kubeconfig: %w", err)
	}
	return &kubeletJoin{hostname: ign.Hostname, bootstrap: cfg}, nil
}

func (j *kubeletJoin) done() bool { return j.servingCert != nil }

func (j *kubeletJoin) nodeName() string { return "system:node:" + j.hostname }

// step advances the join by at most one stage and reports what happened.
func (j *kubeletJoin) step(ctx context.Context, opts *options) (string, error) {
	switch {
	case j.clientCert == nil:
		return j.stepClientCert(ctx)
	case !j.nodeRegistered:
		return j.stepRegisterNode(ctx, opts)
	case j.servingCert == nil:
		return j.stepServingCert(ctx, opts)
	}
	return "", nil
}

func (j *kubeletJoin) stepClientCert(ctx context.Context) (string, error) {
	cs, err := kubernetes.NewForConfig(j.bootstrap)
	if err != nil {
		return "", err
	}
	if j.clientCSR == "" {
		key, req, err := newCSR(j.nodeName(), nil, nil)
		if err != nil {
			return "", err
		}
		name, err := submitCSR(ctx, cs, req, certificatesv1.KubeAPIServerClientKubeletSignerName,
			[]certificatesv1.KeyUsage{certificatesv1.UsageDigitalSignature, certificatesv1.UsageClientAuth})
		if err != nil {
			return "", fmt.Errorf("submit client CSR with bootstrap credentials: %w", err)
		}
		j.clientKey, j.clientCSR = key, name
		return fmt.Sprintf("submitted client CSR %s", name), nil
	}
	cert, err := issuedCert(ctx, cs, j.clientCSR)
	if err != nil || cert == nil {
		return "", err
	}
	j.clientCert = cert
	return fmt.Sprintf("client CSR %s approved and issued", j.clientCSR), nil
}

func (j *kubeletJoin) nodeClient() (*kubernetes.Clientset, error) {
	keyDER, err := x509.MarshalECPrivateKey(j.clientKey)
	if err != nil {
		return nil, err
	}
	cfg := rest.AnonymousClientConfig(j.bootstrap)
	cfg.CertData = j.clientCert
	cfg.KeyData = pem.EncodeToMemory(&pem.Block{Type: "EC PRIVATE KEY", Bytes: keyDER})
	return kubernetes.NewForConfig(cfg)
}

func (j *kubeletJoin) stepRegisterNode(ctx context.Context, opts *options) (string, error) {
	cs, err := j.nodeClient()
	if err != nil {
		return "", err
	}
	node := &corev1.Node{
		ObjectMeta: metav1.ObjectMeta{
			Name: j.hostname,
			Labels: map[string]string{
				corev1.LabelHostname:   j.hostname,
				corev1.LabelArchStable: "arm64",
				corev1.LabelOSStable:   "linux",
				fakeNodeLabel:          "true",
			},
			// kwok keeps the Node Ready and runs its pods.
			Annotations: map[string]string{"kwok.x-k8s.io/node": "fake"},
		},
	}
	_, err = cs.CoreV1().Nodes().Create(ctx, node, metav1.CreateOptions{})
	if err != nil && !apierrors.IsAlreadyExists(err) {
		return "", fmt.Errorf("register Node as %s: %w", j.nodeName(), err)
	}
	if err := setSimulatedNodeStatus(ctx, cs, node.Name, opts); err != nil {
		return "", fmt.Errorf("update Node status as %s: %w", j.nodeName(), err)
	}
	j.nodeRegistered = true
	return fmt.Sprintf("registered Node %s with its own client certificate", j.hostname), nil
}

func (j *kubeletJoin) stepServingCert(ctx context.Context, opts *options) (string, error) {
	cs, err := j.nodeClient()
	if err != nil {
		return "", err
	}
	if j.servingCSR == "" {
		key, req, err := newCSR(j.nodeName(), []string{j.hostname}, []net.IP{net.ParseIP(opts.nodeIP)})
		if err != nil {
			return "", err
		}
		name, err := submitCSR(ctx, cs, req, certificatesv1.KubeletServingSignerName,
			[]certificatesv1.KeyUsage{certificatesv1.UsageDigitalSignature, certificatesv1.UsageServerAuth})
		if err != nil {
			return "", fmt.Errorf("submit serving CSR as %s: %w", j.nodeName(), err)
		}
		j.servingKey, j.servingCSR = key, name
		return fmt.Sprintf("submitted serving CSR %s", name), nil
	}
	cert, err := issuedCert(ctx, cs, j.servingCSR)
	if err != nil || cert == nil {
		return "", err
	}
	j.servingCert = cert
	return fmt.Sprintf("serving CSR %s approved and issued; join complete", j.servingCSR), nil
}

func newCSR(commonName string, dnsNames []string, ips []net.IP) (*ecdsa.PrivateKey, []byte, error) {
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return nil, nil, err
	}
	der, err := x509.CreateCertificateRequest(rand.Reader, &x509.CertificateRequest{
		Subject:     pkix.Name{CommonName: commonName, Organization: []string{"system:nodes"}},
		DNSNames:    dnsNames,
		IPAddresses: ips,
	}, key)
	if err != nil {
		return nil, nil, err
	}
	return key, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE REQUEST", Bytes: der}), nil
}

func submitCSR(ctx context.Context, cs kubernetes.Interface, req []byte, signer string, usages []certificatesv1.KeyUsage) (string, error) {
	csr, err := cs.CertificatesV1().CertificateSigningRequests().Create(ctx, &certificatesv1.CertificateSigningRequest{
		ObjectMeta: metav1.ObjectMeta{GenerateName: "csr-"},
		Spec: certificatesv1.CertificateSigningRequestSpec{
			Request:    req,
			SignerName: signer,
			Usages:     usages,
		},
	}, metav1.CreateOptions{})
	if err != nil {
		return "", err
	}
	return csr.Name, nil
}

// issuedCert returns the certificate once the CSR is approved and signed, nil
// while it is pending, and an error if it was denied or failed.
func issuedCert(ctx context.Context, cs kubernetes.Interface, name string) ([]byte, error) {
	csr, err := cs.CertificatesV1().CertificateSigningRequests().Get(ctx, name, metav1.GetOptions{})
	if err != nil {
		return nil, err
	}
	for _, c := range csr.Status.Conditions {
		if c.Type == certificatesv1.CertificateDenied || c.Type == certificatesv1.CertificateFailed {
			return nil, fmt.Errorf("CSR %s %s: %s", name, c.Type, c.Message)
		}
	}
	if len(csr.Status.Certificate) == 0 {
		return nil, nil
	}
	return csr.Status.Certificate, nil
}

// setSimulatedNodeStatus reports what a real DPU Node reports and kwok does
// not: device resources and an address outside the hosted cluster network.
// kwok updates the same status, so retry on conflict.
func setSimulatedNodeStatus(ctx context.Context, cs kubernetes.Interface, name string, opts *options) error {
	return retry.RetryOnConflict(retry.DefaultRetry, func() error {
		node, err := cs.CoreV1().Nodes().Get(ctx, name, metav1.GetOptions{})
		if err != nil {
			return err
		}
		if node.Status.Capacity == nil {
			node.Status.Capacity = corev1.ResourceList{}
		}
		if node.Status.Allocatable == nil {
			node.Status.Allocatable = corev1.ResourceList{}
		}
		for k, v := range simulatedResources(opts) {
			node.Status.Capacity[k] = v
			node.Status.Allocatable[k] = v
		}
		node.Status.NodeInfo.Architecture = "arm64"
		node.Status.NodeInfo.OperatingSystem = "linux"
		node.Status.Addresses = []corev1.NodeAddress{
			{Type: corev1.NodeInternalIP, Address: opts.nodeIP},
			{Type: corev1.NodeHostName, Address: name},
		}
		_, err = cs.CoreV1().Nodes().UpdateStatus(ctx, node, metav1.UpdateOptions{})
		return err
	})
}

func simulatedResources(opts *options) corev1.ResourceList {
	return corev1.ResourceList{
		corev1.ResourceCPU:    resource.MustParse("16"),
		corev1.ResourceMemory: resource.MustParse("32Gi"),
		corev1.ResourcePods:   resource.MustParse("250"),
		// Normally advertised by the SF device plugin; HBN requests it.
		"nvidia.com/bf_sf": *resource.NewQuantity(int64(opts.numSFs), resource.DecimalSI),
	}
}

// simulatedNodeStatusSet reports whether setSimulatedNodeStatus already ran.
func simulatedNodeStatusSet(node *corev1.Node, opts *options) bool {
	q, ok := node.Status.Allocatable["nvidia.com/bf_sf"]
	if !ok || q.Value() != int64(opts.numSFs) {
		return false
	}
	for _, a := range node.Status.Addresses {
		if a.Type == corev1.NodeInternalIP {
			return a.Address == opts.nodeIP
		}
	}
	return false
}
