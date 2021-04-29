use super::channel::{
    consts, ArrayLength, Channel, ChannelReceive, ChannelReceiver, ChannelSend, ChannelSender,
};
use super::signal::{SignalFuture, SignalSlot};
use core::cell::UnsafeCell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use embassy::util::DropBomb;
use generic_array::GenericArray;

/// Trait that each actor must implement.
pub trait Actor: Sized {
    /// Request queue size;
    #[rustfmt::skip]
    type MaxQueueSize<'a>: ArrayLength<ActorMessage<'a, Self>> + ArrayLength<SignalSlot<Self::Response<'a>>> + 'a where Self: 'a = consts::U1;

    /// Notify queue size;
    //    #[rustfmt::skip]
    //    type MaxNotifyQueueSize<'a>: ArrayLength<NotifyMessage<'a, Self>> + 'a where Self: 'a = consts::U0;

    /// The configuration that this actor will expect when mounted.
    type Configuration = ();

    /// The message type that this actor will handle in `on_message`.
    type Message<'a>: Sized
    where
        Self: 'a,
    = ();

    type Response<'a>: Send + Copy
    where
        Self: 'a,
    = ();

    /// The future type returned in `on_start`, usually derived from an `async move` block
    /// in the implementation
    type OnStartFuture<'a>: Future<Output = ()>
    where
        Self: 'a;

    /// The future type returned in `on_message`, usually derived from an `async move` block
    /// in the implementation
    type OnMessageFuture<'a>: Future<Output = Self::Response<'a>>
    where
        Self: 'a;

    /// Called to mount an actor into the system.
    ///
    /// The actor will be presented with both its own `Address<...>`.
    ///
    /// The default implementation does nothing.
    fn on_mount(&mut self, _: Self::Configuration) {}

    /// Lifecycle event of *start*.
    fn on_start(self: Pin<&'_ mut Self>) -> Self::OnStartFuture<'_>;

    /// Handle an incoming message for this actor.
    fn on_message<'m>(
        self: Pin<&'m mut Self>,
        message: &'m mut Self::Message<'m>,
    ) -> Self::OnMessageFuture<'m>;
}

/// A handle to another actor for dispatching messages.
///
/// Individual actor implementations may augment the `Address` object
/// when appropriate bounds are met to provide method-like invocations.
pub struct Address<'a, A: Actor> {
    state: &'a ActorContext<'a, A>,
}

impl<'a, A: Actor> Address<'a, A> {
    pub fn new(state: &'a ActorContext<'a, A>) -> Self {
        Self { state }
    }
}

impl<'a, A: Actor> Address<'a, A> {
    /// Perform an unsafe _async_ message send to the actor behind this address.
    ///
    /// The returned future will be driven to completion by the actor processing the message,
    /// and will complete when the receiving actor have processed the message.
    ///
    /// # Panics
    /// While the request message may contain non-static references, the user must
    /// ensure that the response to the request is fully `.await`'d before returning.
    /// Leaving an in-flight request dangling while references have gone out of lifetime
    /// scope will result in a panic.
    pub fn process<'m>(&self, message: &'m mut A::Message<'m>) -> ProcessFuture<'a, 'm, A>
    where
        'a: 'm,
    {
        self.state.process(message)
    }

    /// Perform an unsafe _async_ message notification to the actor behind this address.
    ///
    /// The returned future will be driven to completion by the actor processing the message,
    /// and will complete when the message have been enqueued, _before_ the message have been
    /// processed.
    ///
    /// # Panics
    /// While the request message may contain non-static references, the user must
    /// ensure that the response to the request is fully `.await`'d before returning.
    /// Leaving an in-flight request dangling while references have gone out of lifetime
    /// scope will result in a panic.
    pub fn notify<'m>(&self, message: A::Message<'a>) -> NotifyFuture<'a, 'm, A>
    where
        'a: 'm,
    {
        self.state.notify(message)
    }
}

impl<'a, A: Actor> Copy for Address<'a, A> {}

impl<'a, A: Actor> Clone for Address<'a, A> {
    fn clone(&self) -> Self {
        Self { state: self.state }
    }
}

pub struct MessageChannel<'a, T, N>
where
    N: ArrayLength<T>,
{
    channel: UnsafeCell<Channel<T, N>>,
    channel_sender: UnsafeCell<Option<ChannelSender<'a, T, N>>>,
    channel_receiver: UnsafeCell<Option<ChannelReceiver<'a, T, N>>>,
}

impl<'a, T, N> MessageChannel<'a, T, N>
where
    N: ArrayLength<T>,
{
    pub fn new() -> Self {
        Self {
            channel: UnsafeCell::new(Channel::new()),
            channel_sender: UnsafeCell::new(None),
            channel_receiver: UnsafeCell::new(None),
        }
    }

    pub fn initialize(&'a self) {
        let (sender, receiver) = unsafe { &mut *self.channel.get() }.split();
        unsafe { &mut *self.channel_sender.get() }.replace(sender);
        unsafe { &mut *self.channel_receiver.get() }.replace(receiver);
    }

    pub fn send<'m>(&self, message: T) -> ChannelSend<'m, 'a, T, N> {
        let sender = unsafe { &mut *self.channel_sender.get() }.as_mut().unwrap();
        sender.send(message)
    }

    pub fn receive<'m>(&self) -> ChannelReceive<'m, 'a, T, N> {
        let receiver = unsafe { &*self.channel_receiver.get() }.as_ref().unwrap();
        receiver.receive()
    }
}

pub struct ActorContext<'a, A: Actor> {
    pub actor: UnsafeCell<A>,
    request_channel: MessageChannel<'a, ActorMessage<'a, A>, A::MaxQueueSize<'a>>,
    signals: UnsafeCell<GenericArray<SignalSlot<A::Response<'a>>, A::MaxQueueSize<'a>>>,
}

impl<'a, A: Actor> ActorContext<'a, A> {
    pub fn new(actor: A) -> Self {
        Self {
            actor: UnsafeCell::new(actor),
            request_channel: MessageChannel::new(),
            signals: UnsafeCell::new(Default::default()),
        }
    }

    /// Launch the actor main processing loop that never returns.
    pub async fn start(&'a self, _: embassy::executor::Spawner)
    where
        A: Unpin,
    {
        let actor = unsafe { Pin::new_unchecked(&mut *self.actor.get()) };

        actor.on_start().await;

        crate::log_stack!();
        loop {
            let message = self.request_channel.receive().await;
            let actor = unsafe { Pin::new_unchecked(&mut *self.actor.get()) };
            match message {
                ActorMessage::Send(message, signal) => {
                    crate::log_stack!();
                    let value = actor.on_message(unsafe { &mut *message }).await;
                    unsafe { &*signal }.signal(value);
                }
                ActorMessage::Notify(mut message) => {
                    crate::log_stack!();
                    // Note: we know that the message sender will panic if it doesn't await the completion
                    // of the message, thus doing a transmute to pretend that message matches the lifetime
                    // of the receiver should be fine...
                    actor
                        .on_message(unsafe { core::mem::transmute(&mut message) })
                        .await;
                }
            }
        }
    }

    /// Acquire a signal slot if there are any free available
    fn acquire_signal(&self) -> &SignalSlot<A::Response<'a>> {
        let signals = unsafe { &mut *self.signals.get() };
        let mut i = 0;
        while i < signals.len() {
            if signals[i].acquire() {
                return &signals[i];
            }
            i += 1;
        }
        panic!("not enough signals!");
    }

    /// Process a message for this actor. The returned future _must_ be awaited before dropped. If it is not
    /// awaited, it will panic.
    fn process<'m>(&'a self, message: &'m mut A::Message<'m>) -> ProcessFuture<'a, 'm, A>
    where
        'a: 'm,
    {
        let signal = self.acquire_signal();
        let message = unsafe { core::mem::transmute::<_, &'a mut A::Message<'a>>(message) };
        let message = ActorMessage::new_send(message, signal);
        let chan = self.request_channel.send(message);
        let sig = SignalFuture::new(signal);
        ProcessFuture::new(chan, sig)
    }

    /// Perform a notification on this actor. The returned future _must_ be awaited before dropped. If it is not
    /// awaited, it will panic.
    fn notify<'m>(&'a self, message: A::Message<'a>) -> NotifyFuture<'a, 'm, A>
    where
        'a: 'm,
    {
        let message = ActorMessage::new_notify(message);

        let chan = self.request_channel.send(message);
        NotifyFuture::new(chan)
    }

    /// Mount the underloying actor and initialize the channel.
    pub fn mount(&'a self, config: A::Configuration) -> Address<'a, A> {
        unsafe { &mut *self.actor.get() }.on_mount(config);
        self.request_channel.initialize();
        Address::new(self)
    }
}

#[derive(PartialEq, Eq)]
enum ProcessState {
    WaitChannel,
    WaitSignal,
}

pub struct NotifyFuture<'a, 'm, A: Actor + 'a> {
    channel: ChannelSend<'m, 'a, ActorMessage<'a, A>, A::MaxQueueSize<'a>>,
    bomb: Option<DropBomb>,
}

impl<'a, 'm, A: Actor> NotifyFuture<'a, 'm, A> {
    pub fn new(channel: ChannelSend<'m, 'a, ActorMessage<'a, A>, A::MaxQueueSize<'a>>) -> Self {
        Self {
            channel,
            bomb: Some(DropBomb::new()),
        }
    }
}

impl<'a, 'm, A: Actor> Future for NotifyFuture<'a, 'm, A> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let result = Pin::new(&mut self.channel).poll(cx);
        if result.is_ready() {
            self.bomb.take().unwrap().defuse();
        }
        result
    }
}

pub struct ProcessFuture<'a, 'm, A: Actor + 'a> {
    channel: ChannelSend<'m, 'a, ActorMessage<'a, A>, A::MaxQueueSize<'a>>,
    signal: SignalFuture<'a, 'm, A::Response<'a>>,
    state: ProcessState,
    bomb: Option<DropBomb>,
}

impl<'a, 'm, A: Actor> ProcessFuture<'a, 'm, A> {
    pub fn new(
        channel: ChannelSend<'m, 'a, ActorMessage<'a, A>, A::MaxQueueSize<'a>>,
        signal: SignalFuture<'a, 'm, A::Response<'a>>,
    ) -> Self {
        Self {
            channel,
            signal,
            state: ProcessState::WaitChannel,
            bomb: Some(DropBomb::new()),
        }
    }
}

impl<'a, 'm, A: Actor> Future for ProcessFuture<'a, 'm, A> {
    type Output = A::Response<'a>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            match self.state {
                ProcessState::WaitChannel => {
                    let result = Pin::new(&mut self.channel).poll(cx);
                    if result.is_ready() {
                        self.state = ProcessState::WaitSignal;
                    } else {
                        return Poll::Pending;
                    }
                }
                ProcessState::WaitSignal => {
                    let result = Pin::new(&mut self.signal).poll(cx);
                    if result.is_ready() {
                        self.bomb.take().unwrap().defuse();
                        return result;
                    } else {
                        return Poll::Pending;
                    }
                }
            }
        }
    }
}

pub enum ActorMessage<'m, A: Actor + 'm> {
    Send(*mut A::Message<'m>, *const SignalSlot<A::Response<'m>>),
    Notify(A::Message<'m>),
}

impl<'m, A: Actor> ActorMessage<'m, A> {
    fn new_send(message: *mut A::Message<'m>, signal: *const SignalSlot<A::Response<'m>>) -> Self {
        ActorMessage::Send(message, signal)
    }

    fn new_notify(message: A::Message<'m>) -> Self {
        ActorMessage::Notify(message)
    }
}
