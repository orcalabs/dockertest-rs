use dockertest::waitfor::{MessageSource, MessageWait};
use dockertest::{DockerTest, StartPolicy, TestBodySpecification};
use test_log::test;

#[test]
fn test_inject_container_name_ip_through_env_communication() {
    let mut test = DockerTest::new();

    let recv = TestBodySpecification::with_repository("dockertest-rs/coop_recv")
        .set_start_policy(StartPolicy::Strict)
        .set_wait_for(Box::new(MessageWait {
            message: "recv started".to_string(),
            source: MessageSource::Stdout,
            timeout: 10,
        }))
        .set_handle("recv");

    let mut send = TestBodySpecification::with_repository("dockertest-rs/coop_send")
        .set_start_policy(StartPolicy::Strict)
        .set_wait_for(Box::new(MessageWait {
            message: "send success".to_string(),
            source: MessageSource::Stdout,
            timeout: 60,
        }));
    send.inject_container_name("recv", "SEND_TO_IP");

    test.provide_container(recv).provide_container(send);

    test.run(|ops| async move {
        let recv = ops.handle("recv");
        recv.assert_message("coop send message to container", MessageSource::Stdout, 5)
            .await;
    });
}
