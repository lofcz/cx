//! Capture raw PCM from the default local microphone. The recorder belongs to the draft.
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicU16, Ordering},
};
use std::thread::JoinHandle;

pub(super) const MAX_SECONDS: u64 = 300;
const BYTES_PER_SECOND: usize = 32_000;

pub struct Capture {
    child: Child,
    samples: Option<JoinHandle<std::io::Result<Vec<u8>>>>,
    pub level: Arc<AtomicU16>,
}

impl Capture {
    pub fn start() -> Result<Self> {
        const SOX_OUTPUT: &str = "-q -c 1 -r 16000 -e signed-integer -b 16 -L -t raw -";
        #[cfg(target_os = "linux")]
        let recorders = [
            (
                "parec",
                "--raw --format=s16le --rate=16000 --channels=1 --latency-msec=30",
            ),
            ("arecord", "-q -t raw -f S16_LE -r 16000 -c 1"),
            ("rec", SOX_OUTPUT),
        ];
        #[cfg(target_os = "macos")]
        let recorders = [
            ("rec", SOX_OUTPUT),
            (
                "ffmpeg",
                "-nostdin -f avfoundation -i :default -ac 1 -ar 16000 -f s16le pipe:1",
            ),
        ];
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let recorders = [
            (
                "sox",
                "-q -d -c 1 -r 16000 -e signed-integer -b 16 -L -t raw -",
            ),
            ("rec", SOX_OUTPUT),
        ];
        for (program, args) in recorders {
            let mut child = match Command::new(program)
                .args(args.split_ascii_whitespace())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(child) => child,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err).context("Could not start the microphone recorder"),
            };
            let mut stdout = child
                .stdout
                .take()
                .context("Missing microphone output pipe")?;
            let level = Arc::new(AtomicU16::new(0));
            let meter = Arc::clone(&level);
            let samples = std::thread::spawn(move || {
                let mut pcm = Vec::new();
                let mut chunk = [0u8; 2048];
                while pcm.len() < MAX_SECONDS as usize * BYTES_PER_SECOND {
                    let n = stdout.read(&mut chunk)?;
                    if n == 0 {
                        break;
                    }
                    let peak = chunk[..n]
                        .chunks_exact(2)
                        .map(|s| i16::from_le_bytes([s[0], s[1]]).unsigned_abs())
                        .max()
                        .unwrap_or(0);
                    meter.store(peak, Ordering::Relaxed);
                    pcm.extend_from_slice(&chunk[..n]);
                }
                Ok(pcm)
            });
            return Ok(Self {
                child,
                samples: Some(samples),
                level,
            });
        }
        anyhow::bail!(
            "No microphone recorder found. Install SoX, or on Linux install parec or arecord."
        )
    }

    pub fn ended(&self) -> bool {
        self.samples.as_ref().is_some_and(JoinHandle::is_finished)
    }

    pub fn finish(mut self) -> Result<Vec<u8>> {
        if let Some(status) = self.child.try_wait()? {
            ensure!(
                status.success(),
                "Microphone recorder exited ({status}). Check your default input device and microphone permission."
            );
        }
        let _ = self.child.kill();
        let pcm = self
            .samples
            .take()
            .context("Missing microphone reader")?
            .join()
            .map_err(|_| anyhow::anyhow!("Microphone reader stopped"))??;
        wave(&pcm)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wave(pcm: &[u8]) -> Result<Vec<u8>> {
    let len = pcm.len() & !1;
    ensure!(len > 0, "No microphone audio was captured.");
    let mut wav = Vec::with_capacity(44 + len);
    wav.extend(b"RIFF");
    wav.extend((36 + len as u32).to_le_bytes());
    wav.extend(b"WAVEfmt \x10\x00\x00\x00\x01\x00\x01\x00");
    wav.extend(16_000u32.to_le_bytes());
    wav.extend(32_000u32.to_le_bytes());
    wav.extend(b"\x02\x00\x10\x00data");
    wav.extend((len as u32).to_le_bytes());
    wav.extend(&pcm[..len]);
    Ok(wav)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wave_contains_only_complete_mono_samples() {
        assert!(wave(&[]).is_err());
        let wav = wave(&[1, 2, 3]).unwrap();
        assert_eq!(&wav[40..], &[2, 0, 0, 0, 1, 2]);
        assert_eq!(wav.len(), 46);
    }
}
