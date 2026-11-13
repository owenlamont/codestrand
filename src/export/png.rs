use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Instant,
};

use anyhow::{Context, bail};
use bevy::prelude::Image;

use super::{MAX_PENDING_CAPTURES, WAIT_LIMIT};

pub(super) struct Writer {
    sender: mpsc::SyncSender<(PathBuf, Image)>,
    result: mpsc::Receiver<anyhow::Result<()>>,
    pending: VecDeque<PathBuf>,
}

impl Writer {
    pub(super) fn new() -> anyhow::Result<Self> {
        let (sender, frames) =
            mpsc::sync_channel::<(PathBuf, Image)>(MAX_PENDING_CAPTURES);
        let (done, result) = mpsc::sync_channel(MAX_PENDING_CAPTURES);
        thread::Builder::new()
            .name("png-writer".into())
            .spawn(move || {
                for (path, image) in frames {
                    let outcome = encode(&path, image);
                    let failed = outcome.is_err();
                    if done.send(outcome).is_err() || failed {
                        break;
                    }
                }
            })
            .context("start PNG worker")?;
        Ok(Self {
            sender,
            result,
            pending: VecDeque::new(),
        })
    }

    pub(super) fn write(&mut self, path: PathBuf, image: Image) -> anyhow::Result<()> {
        loop {
            match self.result.try_recv() {
                Ok(result) => {
                    self.pending.pop_front();
                    result?;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    bail!("PNG worker died before frame {}", path.display());
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        let queued = path.clone();
        match self.sender.try_send((path, image)) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full((path, _))) => {
                bail!("PNG queue is full for frame {}", path.display());
            }
            Err(mpsc::TrySendError::Disconnected((path, _))) => {
                self.result.recv_timeout(WAIT_LIMIT).with_context(|| {
                    format!("PNG worker disconnected before frame {}", path.display())
                })??;
                bail!("PNG worker stopped before frame {}", path.display());
            }
        }
        self.pending.push_back(queued);
        if self.pending.len() >= MAX_PENDING_CAPTURES {
            let path = self
                .pending
                .pop_front()
                .expect("PNG window has a pending frame");
            self.result
                .recv_timeout(WAIT_LIMIT)
                .with_context(|| format!("wait for PNG frame {}", path.display()))??;
        }
        Ok(())
    }

    pub(super) fn finish(self) -> anyhow::Result<()> {
        drop(self.sender);
        let started = Instant::now();
        for path in self.pending {
            self.result
                .recv_timeout(WAIT_LIMIT.saturating_sub(started.elapsed()))
                .with_context(|| format!("finish PNG frame {}", path.display()))??;
        }
        Ok(())
    }
}

fn encode(path: &Path, image: Image) -> anyhow::Result<()> {
    image
        .try_into_dynamic()
        .with_context(|| format!("convert frame {}", path.display()))?
        .to_rgb8()
        .save(path)
        .with_context(|| format!("write frame {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acknowledgement_bounds_outstanding_frames() {
        let (sender, frames) = mpsc::sync_channel(MAX_PENDING_CAPTURES);
        let (done, result) = mpsc::sync_channel(MAX_PENDING_CAPTURES);
        let encoded = thread::spawn(move || {
            for _ in 0..MAX_PENDING_CAPTURES {
                frames.recv_timeout(WAIT_LIMIT).unwrap();
            }
            for _ in 0..MAX_PENDING_CAPTURES {
                done.send(Ok(())).unwrap();
            }
        });
        let mut worker = Writer {
            sender,
            result,
            pending: VecDeque::new(),
        };
        for index in 0..MAX_PENDING_CAPTURES {
            worker
                .write(format!("frame-{index:05}.png").into(), Image::default())
                .unwrap();
        }
        assert_eq!(worker.pending.len(), MAX_PENDING_CAPTURES - 1);
        worker.finish().unwrap();
        encoded.join().unwrap();
    }

    #[test]
    fn finish_flushes_every_frame_to_its_path() {
        let directory = tempfile::tempdir().unwrap();
        let mut worker = Writer::new().unwrap();
        for index in 0..2_u8 {
            let image = Image {
                data: Some(vec![index, 0, 0, 255]),
                ..Image::default()
            };
            worker
                .write(
                    directory.path().join(format!("frame-{index:05}.png")),
                    image,
                )
                .unwrap();
        }
        worker.finish().unwrap();
        for index in 0..2_u8 {
            let pixels =
                image::open(directory.path().join(format!("frame-{index:05}.png")))
                    .unwrap()
                    .to_rgb8();
            assert_eq!(pixels.get_pixel(0, 0).0, [index, 0, 0]);
        }
    }

    #[test]
    fn worker_reports_the_failed_frame_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missing/frame-00003.png");
        let mut worker = Writer::new().unwrap();
        worker.write(path.clone(), Image::default()).unwrap();
        let error = worker.finish().unwrap_err();
        assert!(format!("{error:#}").contains(&path.display().to_string()));
    }

    #[test]
    fn full_queue_fails_without_waiting() {
        let (sender, _frames) = mpsc::sync_channel(0);
        let (_done, result) = mpsc::channel();
        let mut worker = Writer {
            sender,
            result,
            pending: VecDeque::new(),
        };
        let error = worker
            .write("frame-00004.png".into(), Image::default())
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("queue is full for frame frame-00004.png")
        );
    }

    #[test]
    fn dead_worker_fails_without_waiting() {
        let (sender, frames) = mpsc::sync_channel(1);
        let (done, result) = mpsc::channel();
        drop(frames);
        drop(done);
        let mut worker = Writer {
            sender,
            result,
            pending: VecDeque::new(),
        };
        let error = worker
            .write("frame-00005.png".into(), Image::default())
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("worker died before frame frame-00005.png")
        );
    }
}
