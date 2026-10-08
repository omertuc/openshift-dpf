package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"net/http"
	"time"

	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/types"
	"k8s.io/client-go/dynamic"
)

// runAgent plays the dpu-agent of the DPUs of one host towards a real
// hostagent: on the DPU, after the installed OS boots, the agent reports its
// startup to the hostagent (normally over tmfifo, here over localhost), which
// writes DPU.status.agentStatus.
func runAgent(ctx context.Context, mgmt dynamic.Interface, opts *options) {
	a := &agent{mgmt: mgmt, opts: opts, reported: map[string]bool{}, kicked: map[string]time.Time{}}
	ticker := time.NewTicker(opts.interval)
	defer ticker.Stop()
	for {
		if err := a.reconcile(ctx); err != nil {
			slog.Error("agent", "host", opts.agentHost, "err", err)
		}
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
	}
}

type agent struct {
	mgmt dynamic.Interface
	opts *options
	// reported is keyed by DPU UID: the first startup after an OS install
	// asks for a reboot, like a real first boot.
	reported map[string]bool
	kicked   map[string]time.Time
}

func (a *agent) reconcile(ctx context.Context) error {
	dpus, err := a.mgmt.Resource(dpuGVR).Namespace(a.opts.namespace).List(ctx, metav1.ListOptions{})
	if err != nil {
		return err
	}
	for i := range dpus.Items {
		dpu := &dpus.Items[i]
		host, _, _ := unstructured.NestedString(dpu.Object, "spec", "dpuNodeName")
		phase, _, _ := unstructured.NestedString(dpu.Object, "status", "phase")
		if host != a.opts.agentHost {
			continue
		}
		if phase == "Rebooting" {
			if err := a.kick(ctx, dpu); err != nil {
				return fmt.Errorf("DPU %s: %w", dpu.GetName(), err)
			}
			continue
		}
		if phase != "DPU Config" {
			continue
		}
		// The controller acts on a startup it has not seen yet.
		reported, _, _ := unstructured.NestedString(dpu.Object, "status", "agentStatus", "lastStartupTime")
		seen, _, _ := unstructured.NestedString(dpu.Object, "status", "agentLastStartupTime")
		if reported != "" && reported != seen {
			continue
		}
		method := "NoAction"
		if !a.reported[string(dpu.GetUID())] {
			method = a.opts.firstBootRebootMethod
		}
		if err := postAgentStartup(ctx, a.opts.hostagentURL, dpu, method); err != nil {
			return fmt.Errorf("DPU %s: %w", dpu.GetName(), err)
		}
		a.reported[string(dpu.GetUID())] = true
		slog.Info("reported agent startup to hostagent", "dpu", dpu.GetName(), "rebootMethod", method)
	}
	return nil
}

// kick touches a rebooting DPU now and then: the hostagent notices a finished
// reboot only when it reconciles the DPU, and it does not requeue by itself.
// On a real host the DPU sees enough other updates during a reboot.
func (a *agent) kick(ctx context.Context, dpu *unstructured.Unstructured) error {
	uid := string(dpu.GetUID())
	if time.Since(a.kicked[uid]) < 30*time.Second {
		return nil
	}
	patch := fmt.Sprintf(`{"metadata":{"annotations":{"sim.dpf.openshift.io/kick":%q}}}`, time.Now().UTC().Format(time.RFC3339))
	if _, err := a.mgmt.Resource(dpuGVR).Namespace(dpu.GetNamespace()).Patch(ctx, dpu.GetName(), types.MergePatchType, []byte(patch), metav1.PatchOptions{}); err != nil {
		return err
	}
	a.kicked[uid] = time.Now()
	return nil
}

func postAgentStartup(ctx context.Context, hostagentURL string, dpu *unstructured.Unstructured, rebootMethod string) error {
	body, err := json.Marshal(map[string]any{
		"dpuName":      dpu.GetName(),
		"dpuNamespace": dpu.GetNamespace(),
		"dpuUID":       string(dpu.GetUID()),
		"agentStatus": map[string]any{
			"rebootMethod":    rebootMethod,
			"lastStartupTime": time.Now().UTC().Format(time.RFC3339),
		},
	})
	if err != nil {
		return err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, hostagentURL+"/update-status", bytes.NewReader(body))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode/100 != 2 {
		return fmt.Errorf("POST /update-status: %s", resp.Status)
	}
	return nil
}
