// sf-device-plugin advertises simulated BlueField scalable functions as nvidia.com/bf_sf on a DPU
// node without SF hardware, so DPF services that request SFs (HBN) get scheduled. It runs as a
// static pod on simulated DPUs (dpusim puts it in their ignition); the patched ovs-cni (../ovs-cni)
// plugs the SFs into the pods.
package main

import (
	"context"
	"flag"
	"log/slog"
	"os"
	"os/signal"
	"syscall"
)

func main() {
	count := flag.Int("count", 16, "Simulated SFs to advertise.")
	flag.Parse()

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()
	if err := runSFPlugin(ctx, "nvidia.com/bf_sf", *count); err != nil {
		slog.Error("sf device plugin", "err", err)
		os.Exit(1)
	}
}
