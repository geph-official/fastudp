use concurrent_queue::ConcurrentQueue;

#[derive(Debug)]
pub struct BufferPool {
    cue: ConcurrentQueue<Vec<u8>>,
}

impl BufferPool {
    /// Creates a new buffer pool.
    pub fn new() -> Self {
        Self {
            cue: ConcurrentQueue::bounded(1000),
        }
    }

    /// Obtains something from the buffer pool.
    pub fn alloc(&self, n: usize) -> Vec<u8> {
        if let Ok(mut v) = self.cue.pop() {
            v.resize(n, 0);
            v
        } else {
            vec![0; n]
        }
    }

    /// Places a buffer back into the pool.
    pub fn free(&self, buf: Vec<u8>) {
        if buf.capacity() < 8192 {
            let _ = self.cue.push(buf);
        }
    }
}
