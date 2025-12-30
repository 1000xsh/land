use land_pubsub::SlotSubscriber;

fn main() {
    // create subscriber with 64 slot capacity
    // connects to local solana node websocket endpoint
    let (mut subscriber, mut rx) = SlotSubscriber::new("ws://45.152.160.253:8900", 64);

    // start background websocket thread (consumes the producer internally)
    subscriber.start().expect("failed to start subscriber");

    println!("slot subscriber started, waiting for updates...");

    // event-driven consumer loop
    let mut count = 0;
    loop {
        let n = rx.poll(|info, _seq, _eob| {
            count += 1;
            println!(
                "[{}] slot: {} parent: {} root: {} (connected: {})",
                count,
                info.slot,
                info.parent,
                info.root,
                subscriber.is_connected()
            );
        });

        // if no events, spin loop hint
        if n == 0 {
            std::hint::spin_loop();
        }
    }
}
