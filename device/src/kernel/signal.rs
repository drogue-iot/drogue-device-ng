use atomic_polyfill::{AtomicBool, Ordering};
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use embassy::util::Signal;

pub struct SignalFuture<T: Send> {
    signal: Signal<T>,
}

impl<T: Send> SignalFuture<T> {
    pub fn new() -> Self {
        Self {
            signal: Signal::new(),
        }
    }

    pub fn signal(&self, value: T) {
        self.signal.signal(value)
    }

    pub fn poll_wait(&self, cx: &mut Context<'_>) -> Poll<T> {
        self.signal.poll_wait(cx)
    }
}

impl<T: Send> Unpin for SignalFuture<T> {}

impl<T: Send> Future for SignalFuture<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.signal.poll_wait(cx)
    }
}

impl<T: Send> Drop for SignalFuture<T> {
    fn drop(&mut self) {
        /*
        if !self.signal.signaled() {
            panic!("Signal dropped before done!");
        }*/
    }
}

pub struct SignalSlot<T: Send> {
    free: AtomicBool,
    signal: Signal<T>,
}

impl<T: Send> SignalSlot<T> {
    pub fn acquire(&self) -> bool {
        if self.free.swap(false, Ordering::AcqRel) {
            self.signal.reset();
            true
        } else {
            false
        }
    }

    pub fn poll_wait(&self, cx: &mut Context<'_>) -> Poll<T> {
        self.signal.poll_wait(cx)
    }

    pub fn signal(&self, value: T) {
        self.signal.signal(value)
    }

    pub fn release(&self) {
        self.free.store(true, Ordering::Release)
    }
}

impl<T: Send> Default for SignalSlot<T> {
    fn default() -> Self {
        Self {
            free: AtomicBool::new(true),
            signal: Signal::new(),
        }
    }
}
