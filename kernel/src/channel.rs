use core::{
    cell::{RefCell, UnsafeCell},
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};
use embassy::util::AtomicWaker;
use heapless::spsc::{Consumer, Producer, Queue};
pub use heapless::{consts, ArrayLength};

struct ChannelInner<T, N: ArrayLength<T>> {
    queue: UnsafeCell<Queue<T, N>>,
    producer_waker: AtomicWaker,
    consumer_waker: AtomicWaker,
}

impl<T, N: ArrayLength<T>> Default for ChannelInner<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, N: ArrayLength<T>> ChannelInner<T, N> {
    pub fn new() -> Self {
        Self {
            queue: UnsafeCell::new(Queue::new()),
            producer_waker: AtomicWaker::new(),
            consumer_waker: AtomicWaker::new(),
        }
    }

    fn register_consumer(&self, waker: Waker) {
        self.consumer_waker.register(waker);
    }

    fn register_producer(&self, waker: Waker) {
        self.producer_waker.register(waker);
    }

    fn wake_producer(&self) {
        self.producer_waker.wake();
    }

    fn wake_consumer(&self) {
        self.consumer_waker.wake();
    }

    fn split<'a>(&'a self) -> (ChannelProducer<'a, T, N>, ChannelConsumer<'a, T, N>) {
        let (producer, consumer) = unsafe { (&mut *self.queue.get()).split() };
        (
            ChannelProducer::new(producer, self),
            ChannelConsumer::new(consumer, self),
        )
    }
}

pub struct Channel<T, N: ArrayLength<T>> {
    inner: ChannelInner<T, N>,
}

impl<T, N: ArrayLength<T>> Default for Channel<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, N: ArrayLength<T>> Channel<T, N> {
    pub fn new() -> Self {
        let inner = ChannelInner::new();
        Self { inner }
    }

    pub fn split<'a>(&'a self) -> (ChannelProducer<'a, T, N>, ChannelConsumer<'a, T, N>) {
        self.inner.split()
    }
}

struct ChannelProducer<'a, T, N: ArrayLength<T>> {
    inner: &'a ChannelInner<T, N>,
    producer: Producer<'a, T, N>,
}

impl<'a, T, N: 'a + ArrayLength<T>> ChannelProducer<'a, T, N> {
    pub fn new(producer: Producer<'a, T, N>, inner: &'a ChannelInner<T, N>) -> Self {
        Self { producer, inner }
    }

    fn poll_enqueue(&self, cx: &mut Context<'_>, element: &mut Option<T>) -> Poll<()> {
        if self.producer.ready() {
            let value = element.take().unwrap();
            self.producer.enqueue(value).ok().unwrap();
            self.inner.wake_consumer();
            Poll::Ready(())
        } else {
            self.producer_waker.register(cx.waker());
            Poll::Pending
        }
    }

    pub fn send<'m>(&'m self, value: T) -> ChannelSend<'m, T, N> {
        ChannelSend {
            producer: &self,
            element: Some(value),
        }
    }
}

pub struct ChannelSend<'a, T, N: ArrayLength<T>> {
    producer: &'a ChannelProducer<'a, T, N>,
    element: Option<T>,
}

impl<'a, T, N: ArrayLength<T>> Unpin for ChannelSend<'a, T, N> {}

impl<'a, T, N: ArrayLength<T>> Future for ChannelSend<'a, T, N> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.producer.poll_enqueue(cx, &mut self.element)
    }
}

struct ChannelConsumer<'a, T, N: ArrayLength<T>> {
    inner: &'a ChannelInner<T, N>,
    consumer: Consumer<'a, T, N>,
}

impl<'a, T, N: 'a + ArrayLength<T>> ChannelConsumer<'a, T, N> {
    pub fn new(consumer: Consumer<'a, T, N>, inner: &'a ChannelInner<T, N>) -> Self {
        Self { consumer, inner }
    }

    fn poll_try_dequeue(&self) -> Poll<Option<T>> {
        if let Some(value) = self.consumer.dequeue() {
            self.inner.wake_producer();
            Poll::Ready(Some(value))
        } else {
            Poll::Ready(None)
        }
    }

    fn poll_dequeue(&self, cx: &mut Context<'_>) -> Poll<T> {
        if let Some(value) = self.consumer.dequeue() {
            self.inner.wake_producer();
            Poll::Ready(value)
        } else {
            self.inner.register_consumer(cx.waker());
            Poll::Pending
        }
    }

    pub fn receive<'m>(&'m self) -> ChannelReceive<'m, T, N> {
        ChannelReceive { consumer: &self }
    }
    pub fn try_receive<'m>(&'m self) -> ChannelTryReceive<'m, T, N> {
        ChannelTryReceive { consumer: &self }
    }
}

pub struct ChannelReceive<'a, T, N: ArrayLength<T>> {
    consumer: &'a ChannelConsumer<'a, T, N>,
}

impl<'a, T, N: ArrayLength<T>> Future for ChannelReceive<'a, T, N> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.consumer.poll_dequeue(cx)
    }
}

pub struct ChannelTryReceive<'a, T, N: ArrayLength<T>> {
    consumer: &'a ChannelConsumer<'a, T, N>,
}

impl<'a, T, N: ArrayLength<T>> Future for ChannelTryReceive<'a, T, N> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        self.consumer.poll_try_dequeue()
    }
}
