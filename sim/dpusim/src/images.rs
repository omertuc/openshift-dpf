//! Builds and pushes the patched images the simulation runs. The patches and
//! Containerfiles live next to the other sim/ sources and are compiled in.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use clap::Subcommand;
use tempfile::TempDir;

use crate::cmd::{CommandExt, command};

/// NVIDIA's mock-dms (doca-platform test/mock/dms) patches, applied in order:
/// - 0001: --skip-dpu-cluster-node-selector (m1+: sim-dpu joins the DPU Node
///   from the real ignition instead of mock-dms creating it)
/// - 0002: --ignore-host-selector (m2a+: a real hostagent serves those hosts)
const MOCK_DMS_PATCHES: [&str; 2] = [
    include_str!("../../mock-dms/0001-test-mock-dms-skip-DPU-cluster-Node-creation-for-sel.patch"),
    include_str!("../../mock-dms/0002-test-mock-dms-leave-hosts-served-by-a-real-hostagent.patch"),
];
/// doca-platform public-main the mock-dms patches apply to.
const MOCK_DMS_BASE: &str = "ee931d25a436d77d78b28c38f00170bc190a3ece";

const OVS_CNI_CONTAINERFILE: &str = include_str!("../../ovs-cni/Containerfile");

const OVN_DPF_UTILS_PATCH: &str = include_str!("../../ovn-dpf-utils/sim-dpu.patch");
const OVN_DPF_UTILS_CONTAINERFILE: &str = include_str!("../../ovn-dpf-utils/Containerfile");
const OVN_DPF_UTILS_BASE: &str = "v26.4.1-ocp-release-v4.22";

/// DPF's sfc-controller with the port type from DPF_SIM_PORT_TYPE, which the
/// Containerfile sets to "system" (OVS's kernel datapath on simulated DPUs).
const SFC_CONTROLLER_PATCH: &str = include_str!("../../sfc-controller/sim-port-type.patch");
const SFC_CONTROLLER_CONTAINERFILE: &str = include_str!("../../sfc-controller/Containerfile");
/// The deployed DPF version the sfc-controller patch applies to.
const SFC_CONTROLLER_BASE: &str = "v26.4.1";

const OVNK_CONTAINERFILE: &str = include_str!("../../ovnk/Containerfile");
const OVNK_BINARIES: [&str; 6] = [
    "cmd/ovnkube",
    "cmd/ovn-k8s-cni-overlay",
    "hybrid-overlay/cmd/hybrid-overlay-node",
    "cmd/ovnkube-trace",
    "cmd/ovnkube-identity",
    "cmd/ovnkube-observ",
];

#[derive(Subcommand)]
pub enum ImageCommand {
    /// NVIDIA's mock-dms with the sim/mock-dms patches.
    MockDms {
        #[arg(default_value = "quay.io/otuchfel/mock-dms:m2a")]
        image: String,
        /// doca-platform checkout. Default: ~/repos/doca-platform.
        #[arg(long, env = "DOCA_PLATFORM_DIR")]
        doca_platform_dir: Option<PathBuf>,
    },
    /// DPF's ovs-cni with simulated SFs, for arm64 DPUs.
    OvsCni {
        /// doca-platform checkout at v26.4.1 with sim/ovs-cni/sim-sf.patch applied.
        doca_platform_dir: PathBuf,
        #[arg(default_value = "quay.io/otuchfel/sim-dpu:ovs-cni-simsf-arm64")]
        image: String,
    },
    /// ovn-kubernetes-dpf-utils with the simulated-DPU cniprovisioner (arm64).
    OvnDpfUtils {
        #[arg(default_value = "quay.io/otuchfel/sim-dpu:ovn-dpf-utils-simdpu-arm64")]
        image: String,
        /// ovn-kubernetes-dpf checkout. Default: ~/repos/ovn-kubernetes-dpf.
        #[arg(long, env = "OVNK_DPF_DIR")]
        ovnk_dpf_dir: Option<PathBuf>,
    },
    /// DPF's sfc-controller adding ports as system ports, for arm64 DPUs.
    SfcController {
        #[arg(default_value = "quay.io/otuchfel/sim-dpu:sfc-controller-simport-arm64")]
        image: String,
        /// doca-platform checkout. Default: ~/repos/doca-platform.
        #[arg(long, env = "DOCA_PLATFORM_DIR")]
        doca_platform_dir: Option<PathBuf>,
    },
    /// The simulate-dpu OVN-K DPU image (see sim/ovnk/Containerfile).
    Ovnk {
        /// ovn-kubernetes checkout with --simulate-dpu.
        ovn_kubernetes_dir: PathBuf,
        /// The arm64 DPU OVN-K image from the provisioner's "ovn"
        /// DPUServiceTemplate (.spec.helmChart.values.dpuManifests.image).
        base_image: String,
        target_image: String,
        /// Pull secret for the base image (e.g. the cluster's openshift-config/pull-secret).
        authfile: Option<PathBuf>,
    },
}

pub fn run(image_command: &ImageCommand) -> Result<()> {
    match image_command {
        ImageCommand::MockDms {
            image,
            doca_platform_dir,
        } => build_mock_dms(
            image,
            &home_checkout(doca_platform_dir.as_deref(), "doca-platform")?,
        ),
        ImageCommand::OvsCni {
            doca_platform_dir,
            image,
        } => build_ovs_cni(doca_platform_dir, image),
        ImageCommand::OvnDpfUtils {
            image,
            ovnk_dpf_dir,
        } => build_ovn_dpf_utils(
            image,
            &home_checkout(ovnk_dpf_dir.as_deref(), "ovn-kubernetes-dpf")?,
        ),
        ImageCommand::SfcController {
            image,
            doca_platform_dir,
        } => build_sfc_controller(
            image,
            &home_checkout(doca_platform_dir.as_deref(), "doca-platform")?,
        ),
        ImageCommand::Ovnk {
            ovn_kubernetes_dir,
            base_image,
            target_image,
            authfile,
        } => build_ovnk(
            ovn_kubernetes_dir,
            base_image,
            target_image,
            authfile.as_deref(),
        ),
    }
}

/// `explicit` or ~/repos/<name>.
fn home_checkout(explicit: Option<&Path>, name: &str) -> Result<PathBuf> {
    match explicit {
        Some(path) => Ok(path.to_owned()),
        None => Ok(
            PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?)
                .join("repos")
                .join(name),
        ),
    }
}

/// A detached git worktree, removed again when dropped.
struct Worktree {
    repository: PathBuf,
    path: PathBuf,
}

impl Worktree {
    fn add(repository: &Path, path: PathBuf, revision: &str) -> Result<Self> {
        Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(["worktree", "add", "-q", "--detach"])
            .arg(&path)
            .arg(revision)
            .run()?;
        Ok(Self {
            repository: repository.to_owned(),
            path,
        })
    }

    fn apply_patch(&self, patch: &str) -> Result<()> {
        Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .arg("apply")
            .run_with_stdin(patch.as_bytes())
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        let removal = Command::new("git")
            .arg("-C")
            .arg(&self.repository)
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .run();
        if let Err(error) = removal {
            eprintln!(
                "warning: removing worktree {}: {error:#}",
                self.path.display()
            );
        }
    }
}

fn temp_dir() -> Result<TempDir> {
    tempfile::tempdir().context("creating a temporary build directory")
}

fn write_file(path: &Path, contents: &str) -> Result<()> {
    fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

/// `go build` for linux/arm64, statically linked.
fn go_build_arm64(
    package_dir: &Path,
    output: &Path,
    extra_args: &[&str],
    package: &str,
) -> Result<()> {
    Command::new("go")
        .current_dir(package_dir)
        .env("CGO_ENABLED", "0")
        .env("GOOS", "linux")
        .env("GOARCH", "arm64")
        .arg("build")
        .args(extra_args)
        .arg("-o")
        .arg(output)
        .arg(package)
        .run()
}

/// `podman build --platform linux/arm64 -t <image> <context>` (plus
/// `extra_args`) and push.
fn build_and_push_arm64(context_dir: &Path, image: &str, extra_args: &[OsString]) -> Result<()> {
    Command::new("podman")
        .args(["build", "--platform", "linux/arm64"])
        .args(extra_args)
        .args(["-t", image])
        .arg(context_dir)
        .run()?;
    command("podman", ["push", image]).run()
}

fn build_mock_dms(image: &str, doca_platform: &Path) -> Result<()> {
    let work_dir = temp_dir()?;
    let worktree = Worktree::add(doca_platform, work_dir.path().join("src"), MOCK_DMS_BASE)?;
    MOCK_DMS_PATCHES
        .into_iter()
        .try_for_each(|patch| worktree.apply_patch(patch))?;
    Command::new("podman")
        .current_dir(&worktree.path)
        .args([
            "build",
            "-f",
            "test/mock/dms/Dockerfile",
            "--build-arg",
            "builder_image=docker.io/library/golang:1.26.6",
            "--build-arg",
            "base_image=gcr.io/distroless/static:nonroot",
            "-t",
            image,
            ".",
        ])
        .run()?;
    command("podman", ["push", image]).run()
}

fn build_ovs_cni(doca_platform: &Path, image: &str) -> Result<()> {
    let context_dir = temp_dir()?;
    go_build_arm64(
        &doca_platform.join("third_party/forked/ovs-cni"),
        &context_dir.path().join("ovs"),
        &["-tags", "no_openssl"],
        "./cmd/plugin",
    )?;
    write_file(
        &context_dir.path().join("Containerfile"),
        OVS_CNI_CONTAINERFILE,
    )?;
    build_and_push_arm64(context_dir.path(), image, &[])
}

fn build_ovn_dpf_utils(image: &str, ovnk_dpf: &Path) -> Result<()> {
    let work_dir = temp_dir()?;
    let context_dir = temp_dir()?;
    let worktree = Worktree::add(ovnk_dpf, work_dir.path().join("src"), OVN_DPF_UTILS_BASE)?;
    worktree.apply_patch(OVN_DPF_UTILS_PATCH)?;
    go_build_arm64(
        &worktree.path.join("dpf-utils"),
        &context_dir.path().join("cniprovisioner"),
        &["-trimpath"],
        "./cmd/dpucniprovisioner",
    )?;
    write_file(
        &context_dir.path().join("Containerfile"),
        OVN_DPF_UTILS_CONTAINERFILE,
    )?;
    build_and_push_arm64(context_dir.path(), image, &[])
}

fn build_sfc_controller(image: &str, doca_platform: &Path) -> Result<()> {
    let work_dir = temp_dir()?;
    let context_dir = temp_dir()?;
    let worktree = Worktree::add(
        doca_platform,
        work_dir.path().join("src"),
        SFC_CONTROLLER_BASE,
    )?;
    worktree.apply_patch(SFC_CONTROLLER_PATCH)?;
    // The flags DPF's Makefile (binary-sfc-controller) builds it with.
    go_build_arm64(
        &worktree.path,
        &context_dir.path().join("sfc-controller"),
        &["-buildvcs=false", "-trimpath", "-ldflags=-s -w"],
        "./cmd/sfc-controller",
    )?;
    write_file(
        &context_dir.path().join("Containerfile"),
        SFC_CONTROLLER_CONTAINERFILE,
    )?;
    build_and_push_arm64(context_dir.path(), image, &[])
}

fn build_ovnk(
    ovn_kubernetes: &Path,
    base_image: &str,
    target_image: &str,
    authfile: Option<&Path>,
) -> Result<()> {
    let go_controller = ovn_kubernetes.join("go-controller");
    Command::new("hack/build-go.sh")
        .current_dir(&go_controller)
        .env(
            "GOTOOLCHAIN",
            std::env::var_os("GOTOOLCHAIN").unwrap_or_else(|| "go1.26.6".into()),
        )
        .env("GOOS", "linux")
        .env("GOARCH", "arm64")
        .env("GOFLAGS", "")
        .args(OVNK_BINARIES)
        .run()?;

    let context_dir = temp_dir()?;
    copy_dir_files(
        &go_controller.join("_output/go/bin"),
        &context_dir.path().join("bin"),
    )?;
    write_file(
        &context_dir.path().join("Containerfile"),
        OVNK_CONTAINERFILE,
    )?;
    let source_commit = Command::new("git")
        .arg("-C")
        .arg(ovn_kubernetes)
        .args(["rev-parse", "HEAD"])
        .read()?;
    let auth_args = authfile.map(|path| {
        let mut flag = OsString::from("--authfile=");
        flag.push(path);
        flag
    });
    let build_args = auth_args
        .into_iter()
        .chain([
            "--build-arg".into(),
            format!("BASE_IMAGE={base_image}").into(),
            "--build-arg".into(),
            format!("SOURCE_COMMIT={}", source_commit.trim()).into(),
        ])
        .collect::<Vec<OsString>>();
    build_and_push_arm64(context_dir.path(), target_image, &build_args)
}

fn copy_dir_files(source_dir: &Path, destination_dir: &Path) -> Result<()> {
    fs::create_dir_all(destination_dir)
        .with_context(|| format!("creating {}", destination_dir.display()))?;
    fs::read_dir(source_dir)
        .with_context(|| format!("listing {}", source_dir.display()))?
        .try_for_each(|entry| {
            let source = entry
                .with_context(|| format!("listing {}", source_dir.display()))?
                .path();
            let destination =
                destination_dir.join(source.file_name().context("directory entry has no name")?);
            fs::copy(&source, &destination)
                .map(|_bytes| ())
                .with_context(|| {
                    format!("copying {} to {}", source.display(), destination.display())
                })
        })
}
