#[cfg(test)]
mod test {
    // this is a simple version of test_create_advance_epoch_tx_race
    use std::sync::Arc;
    use msim::tracing::info;
    use sui::test_utils::network::TestClusterBuilder;
    use msim_macros::sim_test;
    use tokio::sync::broadcast;
    use tokio::time::{sleep, timeout};
    use core::time::Duration;

    #[sim_test]
    async fn test_race() {
        // Set up wait points.
        let (change_epoch_delay_tx, _change_epoch_delay_rx) = broadcast::channel(1);
        let change_epoch_delay_tx = Arc::new(change_epoch_delay_tx);
        let (reconfig_delay_tx, _reconfig_delay_rx) = broadcast::channel(1);
        let reconfig_delay_tx = Arc::new(reconfig_delay_tx);

        // // Test code runs in node 1 - node 2 is always a validator.
        // let target_node = 2;
        // register_wait("change_epoch_tx_delay",
        //               target_node,
        //               change_epoch_delay_tx.clone(),
        // );
        // register_wait("reconfig_delay",
        //               target_node,
        //               reconfig_delay_tx.clone()
        // );

        let test_cluster = TestClusterBuilder::new()
            .with_epoch_duration_ms(1000)
            .build()
            .await
            .unwrap();

        test_cluster.wait_for_epoch(None).await;

        // Allow time for paused node to execute change epoch tx via state sync.
        sleep(Duration::from_secs(5)).await;

        // now release the pause, node will find that change epoch tx has already been executed.
        info!("releasing change epoch delay tx");
        change_epoch_delay_tx.send(()).unwrap();

        // proceeded with reconfiguration.
        sleep(Duration::from_secs(1)).await;
        reconfig_delay_tx.send(()).unwrap();

    }

}