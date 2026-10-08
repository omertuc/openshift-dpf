package main

import (
	"context"
	"log/slog"
	"time"

	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/runtime/schema"
)

func metav1ListOptions() metav1.ListOptions { return metav1.ListOptions{} }

// sfcObjects are the per-node objects sfc-controller marks Ready after it
// programmed OVS on the DPU, with the condition it owns besides Ready.
var sfcObjects = []struct {
	gvr       schema.GroupVersionResource
	condition string
}{
	{schema.GroupVersionResource{Group: "svc.dpu.nvidia.com", Version: "v1alpha1", Resource: "serviceinterfaces"}, "ServiceInterfaceReconciled"},
	{schema.GroupVersionResource{Group: "svc.dpu.nvidia.com", Version: "v1alpha1", Resource: "servicechains"}, "ServiceChainReconciled"},
}

// reconcileSFC stands in for sfc-controller on simulated DPU Nodes. There is
// no OVS behind them, so this only records what sfc-controller would report.
func (s *simulator) reconcileSFC(ctx context.Context) {
	nodes, err := s.hosted.Resource(schema.GroupVersionResource{Version: "v1", Resource: "nodes"}).
		List(ctx, metav1.ListOptions{LabelSelector: fakeNodeLabel + "=true"})
	if err != nil {
		slog.Error("list simulated DPU Nodes", "err", err)
		return
	}
	simulated := map[string]bool{}
	for _, n := range nodes.Items {
		simulated[n.GetName()] = true
	}
	for _, o := range sfcObjects {
		list, err := s.hosted.Resource(o.gvr).Namespace(s.opts.namespace).List(ctx, metav1.ListOptions{})
		if err != nil {
			slog.Error("list", "resource", o.gvr.Resource, "err", err)
			continue
		}
		for i := range list.Items {
			obj := &list.Items[i]
			node, _, _ := unstructured.NestedString(obj.Object, "spec", "node")
			if !simulated[node] || isReady(obj) {
				continue
			}
			if err := s.markReady(ctx, o.gvr, obj, o.condition); err != nil {
				slog.Error("mark Ready", "resource", o.gvr.Resource, "name", obj.GetName(), "err", err)
				continue
			}
			slog.Info("marked Ready (sfc-controller stand-in)", "resource", o.gvr.Resource, "name", obj.GetName(), "node", node)
		}
	}
}

func isReady(obj *unstructured.Unstructured) bool {
	gen := obj.GetGeneration()
	observed, _, _ := unstructured.NestedInt64(obj.Object, "status", "observedGeneration")
	if observed != gen {
		return false
	}
	conds, _, _ := unstructured.NestedSlice(obj.Object, "status", "conditions")
	for _, c := range conds {
		m, _ := c.(map[string]any)
		if m["type"] == "Ready" && m["status"] == "True" {
			return true
		}
	}
	return false
}

func (s *simulator) markReady(ctx context.Context, gvr schema.GroupVersionResource, obj *unstructured.Unstructured, condition string) error {
	gen := obj.GetGeneration()
	now := time.Now().UTC().Format(time.RFC3339)
	cond := func(t string) map[string]any {
		return map[string]any{
			"type": t, "status": "True", "reason": "Success", "message": "",
			"lastTransitionTime": now, "observedGeneration": gen,
		}
	}
	if err := unstructured.SetNestedField(obj.Object, gen, "status", "observedGeneration"); err != nil {
		return err
	}
	if err := unstructured.SetNestedSlice(obj.Object, []any{cond("Ready"), cond(condition)}, "status", "conditions"); err != nil {
		return err
	}
	_, err := s.hosted.Resource(gvr).Namespace(obj.GetNamespace()).UpdateStatus(ctx, obj, metav1.UpdateOptions{})
	return err
}

// reconcileNodeStatus applies the simulated device resources and address to
// every simulated DPU Node, including those mock-dms created (M0).
func (s *simulator) reconcileNodeStatus(ctx context.Context) {
	nodes, err := s.hostedCS.CoreV1().Nodes().List(ctx, metav1.ListOptions{LabelSelector: fakeNodeLabel + "=true"})
	if err != nil {
		slog.Error("list simulated DPU Nodes", "err", err)
		return
	}
	for i := range nodes.Items {
		node := &nodes.Items[i]
		if simulatedNodeStatusSet(node, s.opts) {
			continue
		}
		if err := setSimulatedNodeStatus(ctx, s.hostedCS, node.Name, s.opts); err != nil {
			slog.Error("set simulated Node status", "node", node.Name, "err", err)
			continue
		}
		slog.Info("set simulated Node status (device plugin stand-in)", "node", node.Name)
	}
}
