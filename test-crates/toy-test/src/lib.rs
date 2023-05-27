// bz: this is a simple version of test_create_advance_epoch_tx_race

#[cfg(test)]
mod test {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;
    use msim_macros::sim_test;
    use jsonrpsee::server::{RpcModule, ServerBuilder};
    use msim::tracing::info;
    use tokio::sync::broadcast;
    use tokio::task;

    pub async fn run_server() -> anyhow::Result<SocketAddr> {
        let server = ServerBuilder::default()
            .build("10.1.1.1:80".parse::<SocketAddr>()?)
            .await?;

        let mut module = RpcModule::new(());
        module.register_method("validator method", |_, _| Ok("lo"))?;

        let addr = server.local_addr()?;
        let handle = server.start(module)?;

        info!("starting validator node server handler ... ");

        // // In this example we don't care about doing shutdown so let's it run forever.
        // // You may use the `ServerHandle` to shut it down or manage it yourself.
        // tokio::spawn(handle.stopped());

        // we kill this server at the end
        while !handle.is_stopped() {
            info!("i am running");
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        Ok(addr)
    }

    // NOTE: node_id @this files and @run_all_ready() (from info.node()) have different values

    #[sim_test]
    async fn test_toy() {
        msim::runtime::init_logger();

        // Intercept the specified async wait point on a given node, and wait there until a message
        // is sent from the given tx.
        let register_wait = |failpoint, node_id, tx: Arc<broadcast::Sender<()>>| {
            info!("register wait for node id {:?} at {:?}", node_id, failpoint);

            let this_node = msim::task::NodeId(node_id);
            task::spawn(
                async move {
                    if this_node == msim::runtime::NodeHandle::current().id() {
                        let mut rx = tx.subscribe();

                        info!("node id {:?} waiting for test to send continuation signal for {}", node_id, failpoint);
                        rx.recv().await.unwrap();
                        info!("continuing {}", failpoint);


                    }
                }
            );
        };

        // Set up wait points.
        let (change_epoch_delay_tx, _change_epoch_delay_rx) = broadcast::channel(1);
        let change_epoch_delay_tx = Arc::new(change_epoch_delay_tx);
        let (reconfig_delay_tx, _reconfig_delay_rx) = broadcast::channel(1);
        let reconfig_delay_tx = Arc::new(reconfig_delay_tx);

        // register wait tx
        let target_node = 1;
        register_wait(
            "change_epoch_tx_delay",
            target_node,
            change_epoch_delay_tx.clone(),
        );
        register_wait(
            "reconfig_delay",
            target_node,
            reconfig_delay_tx.clone());


        // simulate TestClusterBuilder: we just create one node, since all txs are on node id = 2 in test_create_advance_epoch_tx_race
        // instead of running a sui validator like test_create_advance_epoch_tx_race, we run a jsonrpsee server
        // the normal use of a sui node can be found here:
        // test_utils::authority::start_node() @ sui/crates/test-utils/src/authority.rs:106
        let ip = std::net::IpAddr::from_str("10.1.1.1").unwrap();
        let handle = msim::runtime::Handle::current();
        let builder = handle.create_node();
        let node = builder
            .ip(ip)
            .name("validator")
            .init(|| async {
                info!("validator restarted");
            })
            .build();

        // let node run
        node.spawn(async move {
            run_server().await.unwrap();
        });

        // wait til node fully started
        tokio::time::sleep(Duration::from_secs(1)).await;

        // now release the pause, node will find that change epoch tx has already been executed.
        info!("releasing change epoch delay tx");
        change_epoch_delay_tx.send(()).unwrap();

        // proceeded with reconfiguration.
        tokio::time::sleep(Duration::from_secs(1)).await;
        info!("releasing reconfig epoch delay tx");
        reconfig_delay_tx.send(()).unwrap();

        // wait til all txs have been delivered
        tokio::time::sleep(Duration::from_secs(10)).await;

        // kill the jsonrpsee server node
        msim::runtime::Handle::current().kill(node.id());
    }

    #[sim_test]
    async fn test_toy_instrumented_yield() { // use tokio::yield() to replace
        msim::runtime::init_logger();

        // Intercept the specified async wait point on a given node, and wait there until a message
        // is sent from the given tx.
        let register_wait = |failpoint, node_id, tx: Arc<broadcast::Sender<()>>| {
            info!("register wait for node id {:?} at {:?}", node_id, failpoint);

            let this_node = msim::task::NodeId(node_id);
            task::spawn(
                async move {
                    if this_node == msim::runtime::NodeHandle::current().id() {
                        let mut rx = tx.subscribe();

                        info!("waiting for test to send continuation signal for {}", failpoint);
                        rx.recv().await.unwrap();
                        info!("continuing {}", failpoint);
                    }
                }
            );
        };

        // Set up wait points.
        let (change_epoch_delay_tx, _change_epoch_delay_rx) = broadcast::channel(1);
        let change_epoch_delay_tx = Arc::new(change_epoch_delay_tx);
        let (reconfig_delay_tx, _reconfig_delay_rx) = broadcast::channel(1);
        let reconfig_delay_tx = Arc::new(reconfig_delay_tx);

        // register wait tx
        let target_node = 1;
        register_wait(
            "change_epoch_tx_delay",
            target_node,
            change_epoch_delay_tx.clone(),
        );
        register_wait(
            "reconfig_delay",
            target_node,
            reconfig_delay_tx.clone());


        // simulate TestClusterBuilder: we just create one node, since all txs are on node id = 2 in test_create_advance_epoch_tx_race
        // instead of running a sui validator like test_create_advance_epoch_tx_race, we run a jsonrpsee server
        let ip = std::net::IpAddr::from_str("10.1.1.1").unwrap();
        let handle = msim::runtime::Handle::current();
        let builder = handle.create_node();
        let node = builder
            .ip(ip)
            .name("validator")
            .init(|| async {
                info!("validator restarted");
            })
            .build();

        // let node run
        node.spawn(async move {
            run_server().await.unwrap();
        });

        // wait til node fully started
        tokio::time::sleep(Duration::from_secs(1)).await;

        // now release the pause, node will find that change epoch tx has already been executed.
        info!("releasing change epoch delay tx");
        change_epoch_delay_tx.send(()).unwrap();

        // proceeded with reconfiguration.
        tokio::time::sleep(Duration::from_secs(1)).await;
        info!("releasing reconfig epoch delay tx");
        reconfig_delay_tx.send(()).unwrap();

        // wait til all txs have been delivered
        tokio::time::sleep(Duration::from_secs(10)).await;

        // kill the jsonrpsee server node
        msim::runtime::Handle::current().kill(node.id());
    }

}
