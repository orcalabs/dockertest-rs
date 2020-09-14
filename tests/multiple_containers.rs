use dockertest::waitfor::{MessageSource, MessageWait};
use dockertest::{Composition, DockerTest, StartPolicy};
use test_env_log::test;

#[test]
fn test_inject_container_name_ip_through_env_communication() {
    let mut test = DockerTest::new();

    let recv = Composition::with_repository("dockertest-rs/coop_recv")
        .with_start_policy(StartPolicy::Strict)
        .with_wait_for(Box::new(MessageWait {
            message: "recv started".to_string(),
            source: MessageSource::Stdout,
            timeout: 10,
        }))
        .with_container_name("recv");
    test.add_composition(recv);

    let mut send = Composition::with_repository("dockertest-rs/coop_send")
        .with_start_policy(StartPolicy::Strict)
        .with_wait_for(Box::new(MessageWait {
            message: "send success".to_string(),
            source: MessageSource::Stdout,
            timeout: 60,
        }));
    send.inject_container_name("recv", "SEND_TO_IP");
    test.add_composition(send);

    test.run(|ops| async move {
        let recv = ops.handle("recv");
        recv.assert_message("coop send message to container", MessageSource::Stdout, 5)
            .await;
    });
}
