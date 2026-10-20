// Real mutual proof and owned heartbeat lifecycle across the fixture processes.
// The TCP stream replaces SSH only; no context or successful lease is fabricated.
use super::*;

pub(super) struct Control {
    stop: tokio::sync::watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<Result<(), ApiError>>>,
}

impl Control {
    pub(super) async fn connect(channel: &FederationChannel, port: u16) -> Self {
        let mut stream: FederationControlIo = Box::new(
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap(),
        );
        let (context, guard) = tokio::time::timeout(
            phase_sync::DEADLOCK_GUARD,
            federation_attest_central(channel, &mut stream),
        )
        .await
        .unwrap()
        .unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let (ready, received) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(federation_control_central(
            channel.clone(),
            stream,
            context,
            guard,
            stopped,
            ready,
        ));
        let control = Self {
            stop,
            task: Some(task),
        };
        tokio::time::timeout(phase_sync::DEADLOCK_GUARD, received)
            .await
            .unwrap()
            .unwrap();
        control
    }

    pub(super) async fn finish(mut self) {
        self.stop.send_replace(true);
        // Explicit retirement may have ended the stream already. Join the task
        // so its exact owner guard completes teardown before replacing state.
        let _ = self.task.take().unwrap().await.unwrap();
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
