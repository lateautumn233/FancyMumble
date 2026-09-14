//! Preserve key-frame requests before the default RTCP chain consumes them.

use std::collections::VecDeque;

use rtc::interceptor::{interceptor, Interceptor, Packet, StreamInfo, TaggedPacket};
use rtc::rtcp::payload_feedbacks::full_intra_request::FullIntraRequest;
use rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use rtc::sansio;
use rtc::shared::error::Error;

#[derive(Interceptor)]
pub(super) struct KeyFrameFeedback<P> {
    #[next]
    next: P,
    read_queue: VecDeque<TaggedPacket>,
}

impl<P> KeyFrameFeedback<P> {
    pub(super) fn new(next: P) -> Self {
        Self {
            next,
            read_queue: VecDeque::new(),
        }
    }
}

#[interceptor]
impl<P: Interceptor> KeyFrameFeedback<P> {
    #[overrides]
    fn handle_read(&mut self, msg: TaggedPacket) -> Result<(), Self::Error> {
        if let Packet::Rtcp(packets) = &msg.message {
            let requests: Vec<_> = packets
                .iter()
                .filter(|packet| {
                    packet.as_any().is::<PictureLossIndication>()
                        || packet.as_any().is::<FullIntraRequest>()
                })
                .cloned()
                .collect();
            if !requests.is_empty() {
                if self.read_queue.len() == 16 {
                    let _ = self.read_queue.pop_front();
                }
                self.read_queue.push_back(TaggedPacket {
                    now: msg.now,
                    transport: msg.transport,
                    message: Packet::Rtcp(requests),
                });
            }
        }
        self.next.handle_read(msg)
    }

    #[overrides]
    fn poll_read(&mut self) -> Option<Self::Rout> {
        self.read_queue
            .pop_front()
            .or_else(|| self.next.poll_read())
    }

    #[overrides]
    fn close(&mut self) -> Result<(), Self::Error> {
        self.read_queue.clear();
        self.next.close()
    }
}
