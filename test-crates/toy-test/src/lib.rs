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

        let mut module = RpcModule::new(());
        module.register_method("validator method", |_, _| Ok("lo"))?;

        let addr = server.local_addr()?;
        // let handle = server.start(module)?;

        info!("starting validator node server handler 0 ... ");
        tokio::time::sleep(Duration::from_secs(2)).await;
        
        // // In this example we don't care about doing shutdown so let's it run forever.
        // // You may use the `ServerHandle` to shut it down or manage it yourself.
        // tokio::spawn(handle.stopped());

        // // we kill this server at the end
        // if !handle.is_stopped() {
        //     info!("i am running 0");
        //     tokio::time::sleep(Duration::from_secs(1)).await;
        // }

        instrumented_yield().await; // assume we have a fail_point here replaced by instrumented_yield

        info!("i am awake 0");

        Ok(addr)
    }

    pub async fn run_server1() -> anyhow::Result<SocketAddr> {
        let server = ServerBuilder::default()
            .build("10.1.1.1:81".parse::<SocketAddr>()?)
            .await?;

        let mut module = RpcModule::new(());
        module.register_method("validator method", |_, _| Ok("lo"))?;

        let addr = server.local_addr()?;
        // let handle = server.start(module)?;

        info!("starting validator node server handler 1 ... ");
        tokio::time::sleep(Duration::from_secs(2)).await;

        // // In this example we don't care about doing shutdown so let's it run forever.
        // // You may use the `ServerHandle` to shut it down or manage it yourself.
        // tokio::spawn(handle.stopped());

        // // we kill this server at the end
        // if !handle.is_stopped() {
        //     info!("i am running 1");
        //     tokio::time::sleep(Duration::from_secs(1)).await;
        // }

        instrumented_yield().await; // assume we have a fail_point here replaced by instrumented_yield

        info!("i am awake 1");

        Ok(addr)
    }

    pub async fn run_server2() -> anyhow::Result<SocketAddr> {
        let server = ServerBuilder::default()
            .build("10.1.1.2:82".parse::<SocketAddr>()?)
            .await?;

        let mut module = RpcModule::new(());
        module.register_method("validator method", |_, _| Ok("lo"))?;

        let addr = server.local_addr()?;
        // let handle = server.start(module)?;

        info!("starting validator node server handler 2 ... ");
        tokio::time::sleep(Duration::from_secs(2)).await;

        // // In this example we don't care about doing shutdown so let's it run forever.
        // // You may use the `ServerHandle` to shut it down or manage it yourself.
        // tokio::spawn(handle.stopped());

        // // we kill this server at the end
        // if !handle.is_stopped() {
        //     info!("i am running 2");
        //     tokio::time::sleep(Duration::from_secs(1)).await;
        // }

        instrumented_yield().await; // assume we have a fail_point here replaced by instrumented_yield

        info!("i am awake 2");

        Ok(addr)
    }

    pub async fn run_server3() -> anyhow::Result<SocketAddr> {
        let server = ServerBuilder::default()
            .build("10.1.1.2:83".parse::<SocketAddr>()?)
            .await?;

        let mut module = RpcModule::new(());
        module.register_method("validator method", |_, _| Ok("lo"))?;

        let addr = server.local_addr()?;
        // let handle = server.start(module)?;

        info!("starting validator node server handler 3 ... ");
        tokio::time::sleep(Duration::from_secs(2)).await;

        // // In this example we don't care about doing shutdown so let's it run forever.
        // // You may use the `ServerHandle` to shut it down or manage it yourself.
        // tokio::spawn(handle.stopped());

        // // we kill this server at the end
        // if !handle.is_stopped() {
        //     info!("i am running 3");
        //     tokio::time::sleep(Duration::from_secs(1)).await;
        // }

        instrumented_yield().await; // assume we have a fail_point here replaced by instrumented_yield

        info!("i am awake 3");

        Ok(addr)
    }

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

        node.spawn(async move {
            run_server1().await.unwrap();
        });

        let ip2 = std::net::IpAddr::from_str("10.1.1.2").unwrap();
        let builder2 = handle.create_node();
        let node2 = builder2 // builder of type NodeBuilder
            .ip(ip2)
            .name("validator2")
            .init(|| async {
                info!("validator2 restarted");
            })
            .build();

        node2.spawn(async move {
            run_server2().await.unwrap();
        });

        node2.spawn(async move {
            run_server3().await.unwrap();
        });

        // wait til node fully started and enter the instrument_yield()
        tokio::time::sleep(Duration::from_secs(5)).await;
        info!("in test_toy waiting.");

        instrumented_yield().await; // assume we have a fail_point here replaced by instrumented_yield

        tokio::time::sleep(Duration::from_secs(5)).await;
        info!("in test_toy waiting.");

        tokio::time::sleep(Duration::from_secs(5)).await;
        info!("in test_toy waiting.");

        tokio::time::sleep(Duration::from_secs(5)).await;
        info!("in test_toy waiting.");

        // kill the jsonrpsee server node
        msim::runtime::Handle::current().kill(node.id());
        msim::runtime::Handle::current().kill(node2.id());
    }
}
