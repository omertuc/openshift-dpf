//! Running the system tools the simulation drives (ip, ovs-vsctl, podman, ssh, ...).

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// Builds a command from a program and its arguments.
pub fn command<const N: usize>(program: &str, args: [&str; N]) -> Command {
    let mut built = Command::new(program);
    built.args(args);
    built
}

/// The command line, for error messages.
fn describe(command: &Command) -> String {
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|word| word.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

fn check_status(command: &Command, status: std::process::ExitStatus) -> Result<()> {
    if !status.success() {
        bail!("{} exited with {status}", describe(command));
    }
    Ok(())
}

pub trait CommandExt {
    /// Runs it with the terminal's stdio and fails unless it exits 0.
    fn run(&mut self) -> Result<()>;
    /// Runs it with stdout discarded and fails unless it exits 0.
    fn run_silently(&mut self) -> Result<()>;
    /// Runs it with all output discarded, for cleanups whose failure means
    /// there was nothing to clean up. Fails only if it cannot be started.
    fn run_ignoring_failure(&mut self) -> Result<()>;
    /// Runs it with all output discarded and reports whether it exited 0,
    /// for probes like `ip link show <name>`.
    fn succeeds(&mut self) -> Result<bool>;
    /// Runs it and returns its stdout, failing unless it exits 0.
    fn read(&mut self) -> Result<String>;
    /// Runs it with `input` on stdin and fails unless it exits 0.
    fn run_with_stdin(&mut self, input: &[u8]) -> Result<()>;
}

impl CommandExt for Command {
    fn run(&mut self) -> Result<()> {
        let status = self
            .status()
            .with_context(|| format!("running {}", describe(self)))?;
        check_status(self, status)
    }

    fn run_silently(&mut self) -> Result<()> {
        self.stdout(Stdio::null()).run()
    }

    fn run_ignoring_failure(&mut self) -> Result<()> {
        self.succeeds().map(|_succeeded| ())
    }

    fn succeeds(&mut self) -> Result<bool> {
        let status = self
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| format!("running {}", describe(self)))?;
        Ok(status.success())
    }

    fn read(&mut self) -> Result<String> {
        let output = self
            .stderr(Stdio::inherit())
            .output()
            .with_context(|| format!("running {}", describe(self)))?;
        check_status(self, output.status)?;
        String::from_utf8(output.stdout)
            .with_context(|| format!("decoding the output of {}", describe(self)))
    }

    fn run_with_stdin(&mut self, input: &[u8]) -> Result<()> {
        let mut child = self
            .stdin(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting {}", describe(self)))?;
        child
            .stdin
            .take()
            .context("child has no stdin")?
            .write_all(input)
            .with_context(|| format!("writing to the stdin of {}", describe(self)))?;
        let status = child
            .wait()
            .with_context(|| format!("waiting for {}", describe(self)))?;
        check_status(self, status)
    }
}
