use futures::{executor::block_on, future::join};
use lenso::{ProviderStream, StreamInput};
use lenso_kernel::{
    CancellationToken, InvocationContext, NativeStreamItem, NativeStreamSession, RuntimeFailure,
    StreamCapability,
};

#[derive(Debug)]
struct Conversation;

impl StreamCapability for Conversation {
    type OpenRequest = ();
    type Message = String;
    type DomainError = &'static str;

    const ID: &'static str = "example.conversation@1";
    const DESCRIPTOR_VERSION: &'static str = "1.0.0";
}

fn context(request_id: u64) -> InvocationContext {
    InvocationContext::new(request_id, None, CancellationToken::new())
}

#[test]
fn typed_provider_stream_preserves_messages_half_closes_and_domain_terminal() {
    let (stream, mut provider) = ProviderStream::<Conversation>::channel(&context(1), 1);

    block_on(stream.send(Box::new("consumer".to_owned()))).expect("consumer message should fit");
    match block_on(provider.receive()).expect("provider should receive a typed message") {
        StreamInput::Message(message) => assert_eq!(message, "consumer"),
        StreamInput::PeerHalfClosed => panic!("message must not become a half-close"),
    }

    block_on(stream.close_send()).expect("consumer half-close should be admitted");
    assert!(matches!(
        block_on(provider.receive()),
        Ok(StreamInput::PeerHalfClosed)
    ));

    block_on(provider.send("provider".to_owned())).expect("provider message should fit");
    let item = block_on(stream.receive()).expect("consumer should receive one message");
    let NativeStreamItem::Message(message) = item else {
        panic!("provider message must remain a message");
    };
    assert_eq!(
        *message
            .downcast::<String>()
            .expect("generated message type should be preserved"),
        "provider"
    );

    block_on(provider.close_send()).expect("provider half-close should be admitted");
    assert!(matches!(
        block_on(stream.receive()),
        Ok(NativeStreamItem::PeerHalfClosed)
    ));

    block_on(provider.fail("rejected")).expect("domain terminal should be admitted");
    let NativeStreamItem::Terminal(Err(error)) =
        block_on(stream.receive()).expect("consumer should receive the terminal outcome")
    else {
        panic!("domain failure must remain a terminal outcome");
    };
    assert_eq!(
        *error
            .downcast::<&'static str>()
            .expect("generated Domain Error type should be preserved"),
        "rejected"
    );
    assert!(matches!(
        block_on(stream.receive()),
        Err(RuntimeFailure::AdmissionClosed)
    ));
}

#[test]
fn typed_provider_stream_preserves_runtime_failure_and_cancellation() {
    let (runtime_stream, mut runtime_provider) =
        ProviderStream::<Conversation>::channel(&context(2), 1);
    block_on(
        runtime_provider.fail_runtime(RuntimeFailure::ModuleFailure {
            detail: "storage lost".to_owned(),
        }),
    )
    .expect("runtime terminal should be admitted");
    assert!(matches!(
        block_on(runtime_stream.receive()),
        Err(RuntimeFailure::ModuleFailure { detail }) if detail == "storage lost"
    ));

    let (cancelled_stream, mut cancelled_provider) =
        ProviderStream::<Conversation>::channel(&context(3), 1);
    cancelled_stream.cancel();
    assert!(cancelled_provider.is_cancelled());
    assert!(matches!(
        block_on(cancelled_provider.send("late".to_owned())),
        Err(RuntimeFailure::AdmissionClosed)
    ));
    assert!(matches!(
        block_on(cancelled_stream.send(Box::new("late".to_owned()))),
        Err(RuntimeFailure::AdmissionClosed)
    ));
}

#[test]
fn typed_provider_stream_completes_from_one_module_result() {
    block_on(async {
        let (stream, provider) = ProviderStream::<Conversation>::channel(&context(6), 1);
        let completing = provider.complete(Err(lenso::ModuleError::Domain("rejected")));
        let receiving = async {
            assert!(matches!(
                stream.receive().await,
                Ok(NativeStreamItem::PeerHalfClosed)
            ));
            let NativeStreamItem::Terminal(Err(error)) = stream
                .receive()
                .await
                .expect("consumer should receive the terminal outcome")
            else {
                panic!("domain failure must remain a terminal outcome");
            };
            assert_eq!(
                *error
                    .downcast::<&'static str>()
                    .expect("generated Domain Error type should be preserved"),
                "rejected"
            );
        };
        let (completed, ()) = join(completing, receiving).await;
        completed.expect("one-shot completion should be admitted");
    });
}

#[test]
fn typed_provider_stream_rejects_wrong_types_and_duplicate_half_close() {
    let (stream, _provider) = ProviderStream::<Conversation>::channel(&context(4), 1);
    assert!(matches!(
        block_on(stream.send(Box::new(42_u64))),
        Err(RuntimeFailure::ProtocolViolation { capability })
            if capability == Conversation::ID
    ));
    block_on(stream.close_send()).expect("first half-close should be admitted");
    assert!(matches!(
        block_on(stream.close_send()),
        Err(RuntimeFailure::ProtocolViolation { capability })
            if capability == Conversation::ID
    ));
}

#[test]
fn typed_provider_stream_applies_bounded_backpressure_and_wakes_on_cancellation() {
    block_on(async {
        let (stream, mut provider) = ProviderStream::<Conversation>::channel(&context(5), 0);
        {
            let sending = provider.send("bounded".to_owned());
            futures::pin_mut!(sending);
            assert!(futures::poll!(sending.as_mut()).is_pending());

            let (admission, received) = join(sending, stream.receive()).await;
            admission.expect("receiving should release bounded provider admission");
            assert!(matches!(received, Ok(NativeStreamItem::Message(_))));
        }

        let blocked = provider.send("cancelled".to_owned());
        futures::pin_mut!(blocked);
        assert!(futures::poll!(blocked.as_mut()).is_pending());
        stream.cancel();
        assert!(matches!(
            blocked.await,
            Err(RuntimeFailure::AdmissionClosed)
        ));
    });
}
