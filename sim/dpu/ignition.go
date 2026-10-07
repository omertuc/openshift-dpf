package main

import (
	"bytes"
	"compress/gzip"
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"time"
)

// ignitionConfig is the subset of an Ignition config sim-dpu reads.
type ignitionConfig struct {
	Storage struct {
		Files []struct {
			Path     string `json:"path"`
			Contents struct {
				Source      string `json:"source"`
				Compression string `json:"compression"`
			} `json:"contents"`
		} `json:"files"`
	} `json:"storage"`
}

func (c *ignitionConfig) file(path string) ([]byte, error) {
	for _, f := range c.Storage.Files {
		if f.Path != path {
			continue
		}
		data, err := decodeDataURL(f.Contents.Source)
		if err != nil {
			return nil, fmt.Errorf("%s: %w", path, err)
		}
		if f.Contents.Compression == "gzip" {
			r, err := gzip.NewReader(bytes.NewReader(data))
			if err != nil {
				return nil, fmt.Errorf("%s: %w", path, err)
			}
			return io.ReadAll(r)
		}
		return data, nil
	}
	return nil, fmt.Errorf("%s not found in ignition", path)
}

func decodeDataURL(source string) ([]byte, error) {
	rest, ok := strings.CutPrefix(source, "data:")
	if !ok {
		return nil, fmt.Errorf("not a data URL: %.40q", source)
	}
	meta, data, ok := strings.Cut(rest, ",")
	if !ok {
		return nil, fmt.Errorf("malformed data URL")
	}
	if strings.HasSuffix(meta, ";base64") {
		return base64.StdEncoding.DecodeString(data)
	}
	s, err := url.PathUnescape(data)
	return []byte(s), err
}

// dpuIgnition is what a DPU takes from the bf.cfg DPF generated for it. On
// OpenShift the bf.cfg is the provisioner's "live" ignition, which embeds the
// "target" ignition the installed RHCOS boots with.
type dpuIgnition struct {
	Hostname            string
	BootstrapKubeconfig []byte
}

func fetchDPUIgnition(ctx context.Context, registry, bfcfgPath string) (*dpuIgnition, error) {
	u, err := url.JoinPath(registry, bfcfgPath)
	if err != nil {
		return nil, err
	}
	ctx, cancel := context.WithTimeout(ctx, time.Minute)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, u, nil)
	if err != nil {
		return nil, err
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("GET %s: %s", u, resp.Status)
	}
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, err
	}

	var live ignitionConfig
	if err := json.Unmarshal(body, &live); err != nil {
		return nil, fmt.Errorf("bf.cfg is not an ignition config: %w", err)
	}
	hostname, err := live.file("/etc/hostname")
	if err != nil {
		return nil, fmt.Errorf("live ignition: %w", err)
	}
	targetRaw, err := live.file("/var/target.ign")
	if err != nil {
		return nil, fmt.Errorf("live ignition: %w", err)
	}
	var target ignitionConfig
	if err := json.Unmarshal(targetRaw, &target); err != nil {
		return nil, fmt.Errorf("target ignition: %w", err)
	}
	kubeconfig, err := target.file("/etc/kubernetes/kubeconfig")
	if err != nil {
		return nil, fmt.Errorf("target ignition: %w", err)
	}
	return &dpuIgnition{
		Hostname:            strings.TrimSpace(string(hostname)),
		BootstrapKubeconfig: kubeconfig,
	}, nil
}
