use std::io::{self, Read};
use std::process::ExitStatus;
#[cfg(not(windows))]
use std::process::{Child, ChildStderr, ChildStdout};

#[cfg(windows)]
use std::fs::File;
#[cfg(unix)]
use std::os::fd::{AsFd, BorrowedFd};

pub(super) enum ChildPipe {
    #[cfg(not(windows))]
    Stdout(ChildStdout),
    #[cfg(not(windows))]
    Stderr(ChildStderr),
    #[cfg(windows)]
    File(File),
}

impl Read for ChildPipe {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(not(windows))]
            Self::Stdout(pipe) => pipe.read(buffer),
            #[cfg(not(windows))]
            Self::Stderr(pipe) => pipe.read(buffer),
            #[cfg(windows)]
            Self::File(pipe) => pipe.read(buffer),
        }
    }
}

#[cfg(unix)]
impl AsFd for ChildPipe {
    fn as_fd(&self) -> BorrowedFd<'_> {
        match self {
            Self::Stdout(pipe) => pipe.as_fd(),
            Self::Stderr(pipe) => pipe.as_fd(),
        }
    }
}

pub(super) trait ChildProcess {
    fn id(&self) -> u32;
    fn take_stdout(&mut self) -> Option<ChildPipe>;
    fn take_stderr(&mut self) -> Option<ChildPipe>;
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>>;
    fn kill(&mut self) -> io::Result<()>;

    #[cfg(windows)]
    fn terminate_job(&mut self) -> io::Result<bool>;
    #[cfg(windows)]
    fn send_break(&mut self) -> io::Result<bool>;
}

#[cfg(not(windows))]
impl ChildProcess for Child {
    fn id(&self) -> u32 {
        self.id()
    }

    fn take_stdout(&mut self) -> Option<ChildPipe> {
        self.stdout.take().map(ChildPipe::Stdout)
    }

    fn take_stderr(&mut self) -> Option<ChildPipe> {
        self.stderr.take().map(ChildPipe::Stderr)
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.try_wait()
    }

    fn kill(&mut self) -> io::Result<()> {
        self.kill()
    }
}
