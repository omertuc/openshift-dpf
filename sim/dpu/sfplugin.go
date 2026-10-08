package main

import (
	"context"
	"fmt"
	"log/slog"
	"net"
	"os"
	"path/filepath"
	"time"

	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	pluginapi "k8s.io/kubelet/pkg/apis/deviceplugin/v1beta1"
)

// SimSFPrefix marks device IDs of simulated scalable functions. The patched
// ovs-cni turns such an ID into an OVS internal port moved into the pod: one
// netdev that is the SF in the pod and its representor in OVS.
const SimSFPrefix = "sim-sf-"

// sfPlugin advertises simulated BlueField scalable functions (M5) as
// nvidia.com/bf_sf on a DPU without SF hardware, so services like HBN that
// request SFs get scheduled and their CNI gets device IDs.
type sfPlugin struct {
	pluginapi.UnimplementedDevicePluginServer
	resource string
	count    int
	socket   string
}

func runSFPlugin(ctx context.Context, resource string, count int) error {
	p := &sfPlugin{
		resource: resource,
		count:    count,
		socket:   filepath.Join(pluginapi.DevicePluginPath, "sim-bf-sf.sock"),
	}
	for {
		if err := p.serve(ctx); err != nil {
			slog.Error("sf device plugin", "err", err)
		}
		select {
		case <-ctx.Done():
			return nil
		case <-time.After(5 * time.Second):
		}
	}
}

// serve registers with the kubelet and serves until the kubelet restarts
// (its socket is recreated), then returns so the caller registers again.
func (p *sfPlugin) serve(ctx context.Context) error {
	_ = os.Remove(p.socket)
	lis, err := net.Listen("unix", p.socket)
	if err != nil {
		return err
	}
	srv := grpc.NewServer()
	pluginapi.RegisterDevicePluginServer(srv, p)
	go func() { _ = srv.Serve(lis) }()
	defer srv.Stop()

	conn, err := grpc.NewClient("unix://"+pluginapi.KubeletSocket, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		return err
	}
	defer conn.Close()
	if _, err := pluginapi.NewRegistrationClient(conn).Register(ctx, &pluginapi.RegisterRequest{
		Version:      pluginapi.Version,
		Endpoint:     filepath.Base(p.socket),
		ResourceName: p.resource,
	}); err != nil {
		return fmt.Errorf("register %s: %w", p.resource, err)
	}
	slog.Info("registered simulated SFs", "resource", p.resource, "count", p.count)

	kubelet, err := os.Stat(pluginapi.KubeletSocket)
	if err != nil {
		return err
	}
	for {
		select {
		case <-ctx.Done():
			return nil
		case <-time.After(5 * time.Second):
		}
		now, err := os.Stat(pluginapi.KubeletSocket)
		if err != nil || !os.SameFile(kubelet, now) {
			return fmt.Errorf("kubelet restarted, registering again")
		}
		if _, err := os.Stat(p.socket); err != nil {
			return fmt.Errorf("plugin socket removed, registering again")
		}
	}
}

func (p *sfPlugin) GetDevicePluginOptions(context.Context, *pluginapi.Empty) (*pluginapi.DevicePluginOptions, error) {
	return &pluginapi.DevicePluginOptions{}, nil
}

func (p *sfPlugin) ListAndWatch(_ *pluginapi.Empty, stream pluginapi.DevicePlugin_ListAndWatchServer) error {
	devs := make([]*pluginapi.Device, p.count)
	for i := range devs {
		devs[i] = &pluginapi.Device{ID: fmt.Sprintf("%s%d", SimSFPrefix, i), Health: pluginapi.Healthy}
	}
	if err := stream.Send(&pluginapi.ListAndWatchResponse{Devices: devs}); err != nil {
		return err
	}
	<-stream.Context().Done()
	return nil
}

func (p *sfPlugin) Allocate(_ context.Context, req *pluginapi.AllocateRequest) (*pluginapi.AllocateResponse, error) {
	resp := &pluginapi.AllocateResponse{}
	for range req.ContainerRequests {
		resp.ContainerResponses = append(resp.ContainerResponses, &pluginapi.ContainerAllocateResponse{})
	}
	return resp, nil
}

func (p *sfPlugin) PreStartContainer(context.Context, *pluginapi.PreStartContainerRequest) (*pluginapi.PreStartContainerResponse, error) {
	return &pluginapi.PreStartContainerResponse{}, nil
}

func (p *sfPlugin) GetPreferredAllocation(context.Context, *pluginapi.PreferredAllocationRequest) (*pluginapi.PreferredAllocationResponse, error) {
	return &pluginapi.PreferredAllocationResponse{}, nil
}
