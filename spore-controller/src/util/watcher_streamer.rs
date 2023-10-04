use std::pin::Pin;

use futures::{
    ready,
    stream::SelectAll,
    task::{Context, Poll},
    Stream, StreamExt,
};

use tokio::sync::mpsc::{channel, Receiver, Sender};

pub struct WatcherStreamer<T> {
    triggers: SelectAll<T>,
    receiver: Receiver<T>,
}

impl<T: Stream + Unpin> WatcherStreamer<T> {
    pub fn new() -> (Sender<T>, Self) {
        let (sender, receiver) = channel::<T>(10);
        let triggers = SelectAll::<T>::new();
        (sender, Self { triggers, receiver })
    }
}

impl<T: Stream + Unpin> Stream for WatcherStreamer<T> {
    type Item = T::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            let terminated = match self.receiver.poll_recv(cx) {
                Poll::Ready(Some(item)) => {
                    self.triggers.push(item);
                    false
                }
                Poll::Ready(None) => true,
                Poll::Pending => false,
            };
            match ready!(self.triggers.poll_next_unpin(cx)) {
                Some(item) => return Poll::Ready(Some(item)),
                None => {
                    // Only terminate stream if all streams are terminated and receive channel is closed
                    if terminated {
                        return Poll::Ready(None);
                    }
                    return Poll::Pending;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures::stream::BoxStream;
    use tokio::time::timeout;

    use super::*;

    #[tokio::test]
    async fn test_watcher_streamer() {
        let (sender, mut stream) = WatcherStreamer::<BoxStream<i32>>::new();

        timeout(Duration::from_millis(10), stream.next())
            .await
            .expect_err("Stream is not pending");

        assert!(sender
            .send(futures::stream::iter(vec![1, 2]).boxed())
            .await
            .is_ok()); // Cannot use expect or unwrap as the stream is not Debug
        assert_eq!(stream.next().await, Some(1));
        assert_eq!(stream.next().await, Some(2));
        timeout(Duration::from_millis(10), stream.next())
            .await
            .expect_err("Stream is not pending");

        drop(sender);
        assert_eq!(stream.next().await, None);
    }
}
