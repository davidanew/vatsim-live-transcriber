use std::collections::VecDeque;

/// Audio returned from one local VAD update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VadResult {
    pub chunks: Vec<Vec<u8>>,
    pub commit: bool,
}

/// Simple energy-based detector tuned for radio transmissions.
#[derive(Debug)]
pub struct LocalVad {
    threshold: f32,
    required_silent_chunks: usize,
    prefix_capacity: usize,
    prefix: VecDeque<Vec<u8>>,
    active: bool,
    silent_chunks: usize,
}

impl LocalVad {
    pub fn new(threshold: f32, silence_ms: u32, chunk_ms: u32, prefix_ms: u32) -> Self {
        assert!(chunk_ms > 0, "chunk duration must be greater than zero");
        let rounded_chunks = |duration: u32| ((duration + chunk_ms / 2) / chunk_ms).max(1) as usize;
        let prefix_capacity = rounded_chunks(prefix_ms);
        Self {
            threshold,
            required_silent_chunks: rounded_chunks(silence_ms),
            prefix_capacity,
            prefix: VecDeque::with_capacity(prefix_capacity),
            active: false,
            silent_chunks: 0,
        }
    }

    pub fn process(&mut self, pcm: &[u8], rms: f32) -> VadResult {
        if !self.active {
            if self.prefix.len() == self.prefix_capacity {
                self.prefix.pop_front();
            }
            self.prefix.push_back(pcm.to_vec());

            if rms < self.threshold {
                return VadResult {
                    chunks: Vec::new(),
                    commit: false,
                };
            }

            self.active = true;
            self.silent_chunks = 0;
            return VadResult {
                chunks: self.prefix.drain(..).collect(),
                commit: false,
            };
        }

        let chunks = vec![pcm.to_vec()];
        if rms >= self.threshold {
            self.silent_chunks = 0;
            return VadResult {
                chunks,
                commit: false,
            };
        }

        self.silent_chunks += 1;
        if self.silent_chunks < self.required_silent_chunks {
            return VadResult {
                chunks,
                commit: false,
            };
        }

        self.active = false;
        self.silent_chunks = 0;
        self.prefix.clear();
        VadResult {
            chunks,
            commit: true,
        }
    }
}
