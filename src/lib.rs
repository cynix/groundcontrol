//! Process manager designed for container-*like* environments that need
//! to run multiple processes, with basic dependency relationships and
//! pre/post execution commands.

#![forbid(unsafe_code, future_incompatible)]
#![deny(
    missing_debug_implementations,
    nonstandard_style,
    missing_docs,
    unreachable_pub,
    missing_copy_implementations,
    unused_qualifications,
    clippy::unwrap_in_result,
    clippy::unwrap_used
)]

use anyhow::Context;
use async_trait::async_trait;
use config::Config;
use tokio::sync::mpsc;

mod command;
pub mod config;
mod process;

/// Runs a Ground Control specification, returning only when all of the
/// processes have stopped (either because one process triggered a
/// shutdown, or because the `shutdown` signal was triggered).
pub async fn run(config: Config, shutdown: mpsc::UnboundedReceiver<()>) -> anyhow::Result<()> {
    run_processes(config.processes, shutdown)
        .await
        .with_context(|| "Ground Control did not stop cleanly")
}

/// Errors generated when starting processes.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
enum StartProcessError {
    /// Pre-run command failed.
    /// TODO: Rename this to something that indicates that we couldn't even start the process (bad path name or not executable or something?).
    #[error("pre-run command failed")]
    PreRunFailed,

    /// Pre-run command aborted with a non-zero exit code.
    #[error("pre-run command aborted with exit code: {0}")]
    PreRunAborted(i32),

    /// Pre-run command was killed before it could exit.
    #[error("pre-run commadn killed before it could exit")]
    PreRunKilled,

    /// Run command failed.
    #[error("run command failed")]
    RunFailed,
}

/// Starts processes.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
trait StartProcess<MP>: Send + Sync
where
    MP: ManageProcess,
{
    /// Starts the process and returns a handle to the process.
    async fn start_process(
        self,
        process_stopped: mpsc::UnboundedSender<()>,
    ) -> Result<MP, StartProcessError>;
}

/// Errors generated when stopping processes.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
enum StopProcessError {
    /// Stop command failed.
    #[error("stop command failed")]
    StopFailed,

    /// Process aborted with a non-zero exit code.
    #[error("process aborted with exit code: {0}")]
    ProcessAborted(i32),

    /// Process was killed before it could be stopped.
    #[error("process killed before it could be stopped")]
    ProcessKilled,

    /// Post-run command failed.
    #[error("post-run command failed")]
    PostRunFailed,
}

/// Manages started processes.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
trait ManageProcess: Send + Sync {
    /// Stops the process: executes the `stop` command/signal if this is
    /// a daemon process; waits for the process to exit; runs the `post`
    /// command (if present).
    async fn stop_process(self) -> Result<(), StopProcessError>;
}

async fn run_processes<SP, MP>(
    processes: Vec<SP>,
    mut shutdown: mpsc::UnboundedReceiver<()>,
) -> Result<(), StartProcessError>
where
    SP: StartProcess<MP>,
    MP: ManageProcess,
{
    // Create the shutdown channel, which will be used to initiate the
    // shutdown process, regardless of if this is a graceful shutdown
    // triggered by a shutdown signal, or an unexpected shutdown caused
    // by the failure of a daemon process.
    let (shutdown_sender, mut shutdown_receiver) = mpsc::unbounded_channel();

    // Start every process in the order they were found in the config
    // file.
    let mut running: Vec<MP> = Vec::with_capacity(processes.len());
    for sp in processes.into_iter() {
        let process = match sp.start_process(shutdown_sender.clone()).await {
            Ok(process) => process,
            Err(err) => {
                tracing::error!(?err, "Failed to start process; aborting startup procedure");

                // Stop all of the daemon processes that have already
                // started (otherwise they will block Ground Control
                // from exiting and thus the container from shutting
                // down).
                while let Some(process) = running.pop() {
                    if let Err(err) = process.stop_process().await {
                        tracing::error!(?err, "Error stopping process after aborted startup");
                    }
                }

                // Manually drop `shutdown_sender` here, and then drain
                // all of the receiver signals. If we let the channel
                // auto-drop (which happens at the entrance to this
                // match arm), then stopping the already-started
                // processes will generate a bunch of spurious errors,
                // since they will be unable to send their shutdown
                // signals. That also generates out-of-order log lines,
                // since the warnings about those signals may not show
                // up until *after* Ground Control itself thinks it has
                // stopped.
                drop(shutdown_sender);
                while shutdown_receiver.recv().await.is_some() {}

                // Return the original error, now that everything has
                // been stopped.
                return Err(err);
            }
        };

        running.push(process);
    }

    // Convert an external shutdown signal into a shutdown message.
    let external_shutdown_sender = shutdown_sender.clone();
    tokio::spawn(async move {
        // Both sending the shutdown signal, *and dropping the sender,*
        // trigger a shutdown.
        let _ = shutdown.recv().await;
        let _ = external_shutdown_sender.send(());
    });

    tracing::info!(
        process_count = %running.len(),
        "Startup phase completed; waiting for shutdown signal or any process to exit."
    );

    shutdown_receiver
        .recv()
        .await
        .expect("All shutdown senders closed without sending a shutdown signal.");

    // Either one process exited or we received a stop signal; stop all
    // of the processes in the *reverse* order in which they were
    // started.
    tracing::info!("Completion signal triggered; shutting down all processes");

    while let Some(process) = running.pop() {
        // TODO: We could do some sort of thing here where we check to
        // see if this is the process that triggered the shutdown and,
        // *still* `stop` it (since we may need to run `post`), but not
        // actually kill it, since it has already stopped. Basically,
        // just some extra tracking to avoid the WARN log that happens
        // when trying to kill a process that has already exited.
        if let Err(err) = process.stop_process().await {
            tracing::error!(?err, "Error stopping process");
        }
    }

    tracing::info!("All processes have exited.");

    Ok(())
}

#[allow(clippy::unwrap_used)]
#[cfg(test)]
mod test {
    use std::sync::{Arc, Mutex};

    use mockall::Sequence;

    use super::*;

    /// Verifies that a failed `pre` execution aborts all subsequent
    /// command executions.
    #[tokio::test]
    async fn failed_pre_aborts_startup() {
        // Create three mock processes: the first is a daemon process
        // will be started and stopped, the second is a one-shot process
        // that fails to start, the third is never started.
        let mut seq = Sequence::new();

        let mut process_a: MockStartProcess<MockManageProcess> = MockStartProcess::new();
        process_a
            .expect_start_process()
            .once()
            .in_sequence(&mut seq)
            .returning(|_| {
                // We expect this, but do not need to check for it
                // (hence no `once()`); that validation happens in a
                // different test.
                let mut process_a_manager = MockManageProcess::new();
                process_a_manager.expect_stop_process().return_const(Ok(()));
                Ok(process_a_manager)
            });

        let mut process_b: MockStartProcess<MockManageProcess> = MockStartProcess::new();
        process_b
            .expect_start_process()
            .once()
            .in_sequence(&mut seq)
            .return_once(|_| Err(StartProcessError::PreRunFailed));

        let process_c: MockStartProcess<MockManageProcess> = MockStartProcess::new();

        // Run the specification; only `a-pre` should run.
        let spec = vec![process_a, process_b, process_c];
        let (_tx, rx) = mpsc::unbounded_channel();
        let result = run_processes(spec, rx).await;
        assert_eq!(Err(StartProcessError::PreRunFailed), result);
    }

    /// Verifies that a failed `pre` execution shuts down all
    /// previously-started long-running processes.
    #[tokio::test]
    async fn failed_pre_shuts_down_earlier_processes() {
        // Create three mock processes: the first is a daemon process
        // will be started and stopped, the second is a one-shot process
        // that fails to start, the third is never started.
        let mut seq = Sequence::new();

        // This ProcessManager is *last* in the sequence, but is
        // returned by the *first* StartProcess trait in the sequence.
        // We need to pass (a clone) of the manager into the
        // StartProcess closure, but can't initialize the manager until
        // we get to the proper place in the expectation sequence. The
        // solution is to wrap the manager in an Arc-Mutex-Option.
        let process_a_manager: Arc<Mutex<Option<MockManageProcess>>> = Default::default();

        let pam = process_a_manager.clone();
        let mut process_a: MockStartProcess<MockManageProcess> = MockStartProcess::new();
        process_a
            .expect_start_process()
            .once()
            .in_sequence(&mut seq)
            .returning(move |_| Ok(pam.lock().unwrap().take().unwrap()));

        let mut process_b: MockStartProcess<MockManageProcess> = MockStartProcess::new();
        process_b
            .expect_start_process()
            .once()
            .in_sequence(&mut seq)
            .return_once(|_| Err(StartProcessError::PreRunFailed));

        let mut pam = MockManageProcess::new();
        pam.expect_stop_process()
            .once()
            .in_sequence(&mut seq)
            .return_const(Ok(()));
        *process_a_manager.lock().unwrap() = Some(pam);

        let process_c: MockStartProcess<MockManageProcess> = MockStartProcess::new();

        // Run the specification.
        let spec = vec![process_a, process_b, process_c];
        let (_tx, rx) = mpsc::unbounded_channel();
        let result = run_processes(spec, rx).await;
        assert_eq!(Err(StartProcessError::PreRunFailed), result);
    }

    // TODO: Same tests as above, but this time with `run` instead of `pre`.
}
