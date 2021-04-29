use super::{
    channel::{
        consts, ArrayLength, Channel, ChannelReceive, ChannelReceiver, ChannelSend, ChannelSender,
    },
    signal::{SignalFuture, SignalSlot},
    util::ImmediateFuture,
};
use core::cell::UnsafeCell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use embassy::util::DropBomb;
use embassy::util::Signal;

/// Trait that each actor must implement.
pub trait Actor: Sized {
    /// Request queue size;
    #[rustfmt::skip]
    type MaxMessageQueueSize<'a>: ArrayLength<ActorMessage<'a, Self>> + 'a where Self: 'a = consts::U1;

    /// The configuration that this actor will expect when mounted.
    type Configuration = ();

    /// The message type that this actor will handle in `on_message`.
    type Message<'a>: Sized
    where
        Self: 'a,
    = ();

    type Response<'a>: Sized + Send
    where
        Self: 'a,
    = ();

    /// The future type returned in `on_start`, usually derived from an `async move` block
    /// in the implementation
    type OnStartFuture<'a>: Future<Output = ()>
    where
        Self: 'a,
    = ImmediateFuture;

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
        message: Self::Message<'m>,
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
    /// Perform an _async_ message request to the actor behind this address.
    ///
    /// The returned future complete when the receiving actor have processed the
    /// message, and the result from processing is made available when the future
    /// is ready.
    ///
    /// # Panics
    /// While the request message may contain non-static references, the user must
    /// ensure that the response to the request is fully `.await`'d before returning.
    /// Leaving an in-flight request dangling while references have gone out of lifetime
    /// scope will result in a panic.
    #[must_use = "The returned future must be awaited"]
    pub async fn request<'m>(&self, message: A::Message<'m>) -> A::Response<'a>
    where
        'a: 'm,
    {
        #[rustfmt::skip]
        let signal: SignalFuture<A::Response<'a>> = SignalFuture::new();
        unsafe {
            self.state
                .request(message, core::mem::transmute(&signal))
                .await
        }
    }

    /// Perform an _async_ message notification to the actor behind this address.
    ///
    /// The returned future will complete when the message have been enqueued,
    /// _before_ the message have been fully processed.
    ///
    /// # Panics
    /// While the request message may contain non-static references, the user must
    /// ensure that the response to the request is fully `.await`'d before returning.
    /// Leaving an in-flight request dangling while references have gone out of lifetime
    /// scope will result in a panic.
    #[must_use = "The returned future must be awaited"]
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
    channel: MessageChannel<'a, ActorMessage<'a, A>, A::MaxMessageQueueSize<'a>>,
}

impl<'a, A: Actor> ActorContext<'a, A> {
    pub fn new(actor: A) -> Self {
        Self {
            actor: UnsafeCell::new(actor),
            channel: MessageChannel::new(),
        }
    }

    /// Launch the actor main processing loop that never returns.
    pub async fn start(&'a self, _: embassy::executor::Spawner)
    where
        A: Unpin,
    {
        crate::print_value_size("actor", &self.actor);
        crate::print_value_size("channel", &self.channel);
        //        crate::print_value_size("signal", &self.signal);

        let actor = unsafe { Pin::new_unchecked(&mut *self.actor.get()) };

        actor.on_start().await;

        // crate::log_stack!();
        loop {
            // crate::log_stack!();
            let actor = unsafe { Pin::new_unchecked(&mut *self.actor.get()) };
            match self.channel.receive().await {
                ActorMessage::Request(message, signal) => {
                    // crate::log_stack!();
                    let value = actor.on_message(message).await;
                    println!("Message handled, signalling!");
                    signal.signal(value);
                }
                ActorMessage::Notify(message) => {
                    // crate::log_stack!();
                    actor.on_message(message).await;
                }
            }
        }
    }

    /// Perform a request to this actor. The result from processing the request will be provided when the future completes.
    /// The returned future _must_ be awaited before dropped. If it is not
    /// awaited, it will panic.
    unsafe fn request<'m>(
        &'a self,
        message: A::Message<'m>,
        signal: &'m SignalFuture<A::Response<'m>>,
    ) -> RequestFuture<'a, 'm, A>
    where
        'a: 'm,
    {
        // Safety: This is OK because A::Message is Sized.
        let message = core::mem::transmute_copy::<_, A::Message<'a>>(&message);

        // Signal lives just as long as message.
        let signal = core::mem::transmute::<_, &'a SignalFuture<A::Response<'a>>>(signal);

        let message = ActorMessage::Request(message, signal);
        let chan = self.channel.send(message);
        RequestFuture::new(chan, signal)
    }

    /// Perform a notification on this actor. The returned future _must_ be awaited before dropped. If it is not
    /// awaited, it will panic.
    fn notify<'m>(&'a self, message: A::Message<'a>) -> NotifyFuture<'a, 'm, A>
    where
        'a: 'm,
    {
        let message = ActorMessage::Notify(message);

        let chan = self.channel.send(message);
        NotifyFuture::new(chan)
    }

    /// Mount the underloying actor and initialize the channel.
    pub fn mount(&'a self, config: A::Configuration) -> Address<'a, A> {
        unsafe { &mut *self.actor.get() }.on_mount(config);
        self.channel.initialize();
        Address::new(self)
    }
}

#[derive(PartialEq, Eq)]
enum RequestState {
    WaitChannel,
    WaitSignal,
}

pub struct NotifyFuture<'a, 'm, A: Actor + 'a> {
    channel: ChannelSend<'m, 'a, ActorMessage<'a, A>, A::MaxMessageQueueSize<'a>>,
    bomb: Option<DropBomb>,
}

impl<'a, 'm, A: Actor> NotifyFuture<'a, 'm, A> {
    pub fn new(
        channel: ChannelSend<'m, 'a, ActorMessage<'a, A>, A::MaxMessageQueueSize<'a>>,
    ) -> Self {
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

pub struct RequestFuture<'a, 'm, A: Actor + 'a> {
    channel: ChannelSend<'m, 'a, ActorMessage<'a, A>, A::MaxMessageQueueSize<'a>>,
    signal: &'a SignalFuture<A::Response<'a>>,
    state: RequestState,
    bomb: Option<DropBomb>,
}

impl<'a, 'm, A: Actor> RequestFuture<'a, 'm, A> {
    pub fn new(
        channel: ChannelSend<'m, 'a, ActorMessage<'a, A>, A::MaxMessageQueueSize<'a>>,
        signal: &'a SignalFuture<A::Response<'a>>,
    ) -> Self {
        Self {
            channel,
            signal,
            state: RequestState::WaitChannel,
            bomb: Some(DropBomb::new()),
        }
    }
}

impl<'a, 'm, A: Actor> Future for RequestFuture<'a, 'm, A> {
    type Output = A::Response<'a>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            match self.state {
                RequestState::WaitChannel => {
                    let result = Pin::new(&mut self.channel).poll(cx);
                    if result.is_ready() {
                        self.state = RequestState::WaitSignal;
                    } else {
                        return Poll::Pending;
                    }
                }
                RequestState::WaitSignal => {
                    let result = self.signal.poll_wait(cx);
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
    Notify(A::Message<'m>),
    Request(A::Message<'m>, &'m SignalFuture<A::Response<'m>>),
}
