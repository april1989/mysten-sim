// bz: this is a simple version of test_create_advance_epoch_tx_race

#[cfg(test)]
mod test {
    use std::net::SocketAddr;
    use std::time::Duration;
    use msim_macros::sim_test;
    use jsonrpsee::server::{RpcModule, ServerBuilder};
    use msim::tracing::info;
    use msim::task::instrumented_yield;

    pub async fn run_server() -> anyhow::Result<SocketAddr> {
        let server = ServerBuilder::default()
            .build("10.1.1.1:80".parse::<SocketAddr>()?)
            .await?;

        instrumented_yield(); // assume we have a fail_point here replaced by instrumented_yield
        tokio::time::sleep(Duration::from_secs(5)).await;

        let mut module = RpcModule::new(());
        module.register_method("validator method", |_, _| Ok("lo"))?;

        let addr = server.local_addr()?;
        let handle = server.start(module)?;

        info!("starting validator node server handler ... ");

        // // In this example we don't care about doing shutdown so let's it run forever.
        // // You may use the `ServerHandle` to shut it down or manage it yourself.
        // tokio::spawn(handle.stopped());

        // we kill this server at the end
        if !handle.is_stopped() {
            info!("i am running");
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        instrumented_yield(); // assume we have a fail_point here replaced by instrumented_yield

        info!("i am awake");

        Ok(addr)
    }

    // NOTE: node_id @this files and @run_all_ready() (from info.node()) have different values

    #[sim_test]
    async fn test_toy() {
        msim::runtime::init_logger();

        // simulate TestClusterBuilder: we just create one node, since all txs are on node id = 2 in test_create_advance_epoch_tx_race
        // instead of running a sui validator like test_create_advance_epoch_tx_race, we run a jsonrpsee server
        // the normal use of a sui node can be found here:
        // test_utils::authority::start_node() @ sui/crates/test-utils/src/authority.rs:106
        let ip = std::net::IpAddr::from_str("10.1.1.1").unwrap();
        let handle = msim::runtime::Handle::current();
        let builder = handle.create_node();
        let node = builder // builder of type NodeBuilder
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

        // wait til node fully started and enter the instrument_yield()
        tokio::time::sleep(Duration::from_secs(2)).await;
        info!("in test_toy waiting.");

        instrumented_yield(); // assume we have a fail_point here replaced by instrumented_yield

        tokio::time::sleep(Duration::from_secs(2)).await;
        info!("in test_toy waiting.");

        instrumented_yield(); // assume we have a fail_point here replaced by instrumented_yield

        tokio::time::sleep(Duration::from_secs(2)).await;
        info!("in test_toy waiting.");

        tokio::time::sleep(Duration::from_secs(2)).await;
        info!("in test_toy waiting.");

        // kill the jsonrpsee server node
        msim::runtime::Handle::current().kill(node.id());
    }
}
