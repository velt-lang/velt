//! Unix: a JIT host that gets a `SIGSEGV` or a `SIGBUS` says where, on stderr, before it dies: what it
//! touched, the faulting instruction, the thread, and what the reload thread was doing, which
//! tells a fault in the compiler from one in the swapped program (#3194). A crash is otherwise
//! only a signal number.
//!
//! The handler reports once and puts back the handler it replaced (Rust's stack overflow
//! report, or none), so the fault happens again and ends the process as it would have: the
//! supervisor still sees the signal.
//!
//! Both signals: macOS delivers a write to a protected page (read-only code, a guard page) as
//! `SIGBUS`, and only an unmapped address as `SIGSEGV`. Rust's start-up handles both in the host
//! already, so the program's own `SIGBUS` report (std's, of a truncated mapped file, installed
//! only where nothing handles the signal) is not installed in a host with or without this.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// What the reload thread is doing, for the report.
#[derive(Clone, Copy)]
pub enum Step {
    /// Building the changed sources (check, lowering, optimization).
    Building = 1,
    /// Loading the new version's C libraries.
    Loading = 2,
    /// Generating code and swapping it in.
    Swapping = 3,
    /// The reload is answered; the program runs what it left.
    Done = 4,
}

const STEPS: [&str; 5] = [
    "no reload yet",
    "building the change",
    "loading C libraries",
    "generating and swapping code",
    "finished",
];

/// The reload being served, counted from 1 (0: none yet).
static RELOAD: AtomicUsize = AtomicUsize::new(0);
static STEP: AtomicUsize = AtomicUsize::new(0);
/// Where the `velt` executable is loaded: an instruction address minus this is an offset a
/// symbolizer takes. 0 when unknown.
static IMAGE: AtomicUsize = AtomicUsize::new(0);
/// The handlers replaced, per signal of [`SIGNALS`].
static mut PREVIOUS: [std::mem::MaybeUninit<libc::sigaction>; 2] = [
    std::mem::MaybeUninit::zeroed(),
    std::mem::MaybeUninit::zeroed(),
];
const SIGNALS: [libc::c_int; 2] = [libc::SIGSEGV, libc::SIGBUS];

/// A reload request arrived.
pub fn reload_started() {
    RELOAD.fetch_add(1, Ordering::Relaxed);
    at(Step::Building);
}

/// The reload thread is at `step`.
pub fn at(step: Step) {
    STEP.store(step as usize, Ordering::Relaxed);
}

/// Register the handler (once).
pub fn install() {
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::AcqRel) {
        return;
    }
    // SAFETY: `dladdr` reads the loader's tables for an address of this executable; the
    // signal calls are plain, and `PREVIOUS` is written before the handler can run.
    unsafe {
        let mut image: libc::Dl_info = std::mem::zeroed();
        if libc::dladdr(install as *const libc::c_void, &mut image) != 0 {
            IMAGE.store(image.dli_fbase as usize, Ordering::Relaxed);
        }
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_fault as extern "C" fn(_, _, _) as usize;
        // On the thread's alternate stack where it has one (Rust's threads do), so a stack
        // overflow is reported too.
        sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
        libc::sigemptyset(&mut sa.sa_mask);
        for (k, sig) in SIGNALS.into_iter().enumerate() {
            let previous = std::ptr::addr_of_mut!(PREVIOUS[k]).cast::<libc::sigaction>();
            libc::sigaction(sig, &sa, previous);
        }
    }
}

extern "C" fn on_fault(sig: libc::c_int, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    // SAFETY: the kernel passes a valid `siginfo_t` and `ucontext_t` to an `SA_SIGINFO`
    // handler. Everything called is async-signal-safe but macOS's `pthread_getname_np`
    // (`thread_name`).
    unsafe {
        let code = (*info).si_code;
        if !REPORTED.swap(true, Ordering::AcqRel) {
            let mut line = Line::new();
            line.text("velt dev: signal ");
            line.decimal(sig as usize);
            // A fault has a positive code; a signal sent by a process has no address.
            if code > 0 {
                line.text(" touching ");
                line.hex(fault_addr(info));
                let pc = fault_pc(context);
                if pc != 0 {
                    line.text(" at ");
                    line.hex(pc);
                    let image = IMAGE.load(Ordering::Relaxed);
                    if image != 0 {
                        line.text(" (velt is loaded at ");
                        line.hex(image);
                        line.text(")");
                    }
                }
            } else {
                line.text(" sent to the program");
            }
            let mut name = [0u8; 32];
            let named = thread_name(&mut name);
            let len = name.iter().position(|&b| b == 0).unwrap_or(0);
            if named && len > 0 {
                line.text(" on thread `");
                line.bytes(&name[..len]);
                line.text("`");
            }
            line.text("; reload ");
            line.decimal(RELOAD.load(Ordering::Relaxed));
            line.text(": ");
            line.text(STEPS[STEP.load(Ordering::Relaxed).min(STEPS.len() - 1)]);
            line.text("\n");
            line.write();
        }
        let k = usize::from(sig != libc::SIGSEGV);
        let previous = std::ptr::addr_of!(PREVIOUS[k]).cast::<libc::sigaction>();
        libc::sigaction(sig, previous, std::ptr::null_mut());
        // A fault happens again when the handler returns; a sent signal does not.
        if code <= 0 {
            libc::raise(sig);
        }
    }
}

/// The current thread's name into `name` (zero-terminated); false when it cannot be read.
/// Linux asks the kernel (a system call, safe in a handler). Elsewhere `pthread_getname_np`,
/// which is not on the async-signal-safe list: macOS's copies the name out of the thread's own
/// record without taking a lock.
unsafe fn thread_name(name: &mut [u8; 32]) -> bool {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    return libc::prctl(libc::PR_GET_NAME, name.as_mut_ptr()) == 0;
    #[cfg(target_os = "macos")]
    return libc::pthread_getname_np(libc::pthread_self(), name.as_mut_ptr().cast(), 32) == 0;
    #[allow(unreachable_code)]
    false
}

#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe fn fault_addr(info: *mut libc::siginfo_t) -> usize {
    (*info).si_addr() as usize
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
unsafe fn fault_addr(info: *mut libc::siginfo_t) -> usize {
    (*info).si_addr as usize
}

/// The faulting instruction's address, 0 where this target's context is not read.
#[allow(unused_variables)]
unsafe fn fault_pc(context: *mut libc::c_void) -> usize {
    let context = context.cast::<libc::ucontext_t>();
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return (*context).uc_mcontext.gregs[libc::REG_RIP as usize] as usize;
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return (*context).uc_mcontext.pc as usize;
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    return (*(*context).uc_mcontext).__ss.__rip as usize;
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return (*(*context).uc_mcontext).__ss.__pc as usize;
    #[allow(unreachable_code)]
    0
}

/// One report line, built without allocating (the handler may run inside the allocator).
struct Line {
    buf: [u8; 256],
    len: usize,
}

impl Line {
    fn new() -> Line {
        Line {
            buf: [0; 256],
            len: 0,
        }
    }

    fn bytes(&mut self, bytes: &[u8]) {
        let n = bytes.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&bytes[..n]);
        self.len += n;
    }

    fn text(&mut self, text: &str) {
        self.bytes(text.as_bytes());
    }

    fn number(&mut self, mut value: usize, radix: usize) {
        let mut digits = [0u8; 20];
        let mut at = digits.len();
        loop {
            at -= 1;
            digits[at] = b"0123456789abcdef"[value % radix];
            value /= radix;
            if value == 0 {
                break;
            }
        }
        self.bytes(&digits[at..]);
    }

    fn decimal(&mut self, value: usize) {
        self.number(value, 10);
    }

    fn hex(&mut self, value: usize) {
        self.text("0x");
        self.number(value, 16);
    }

    fn write(&self) {
        // SAFETY: writes the initialized part of the buffer to stderr.
        unsafe { libc::write(2, self.buf.as_ptr().cast(), self.len) };
    }
}

/// For tests: fault as a swap that crashes would, by a write to a page that allows none (the
/// address is mapped, so the write is a defined access that the kernel refuses).
pub fn fault() {
    // SAFETY: maps one inaccessible page of its own and writes to it; the write faults.
    unsafe {
        let page = libc::mmap(
            std::ptr::null_mut(),
            4096,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        );
        if page != libc::MAP_FAILED {
            page.cast::<u8>().write_volatile(0);
        }
    }
    std::process::abort();
}

#[cfg(test)]
mod tests {
    use super::Line;

    #[test]
    fn a_line_formats_numbers_and_stops_at_its_end() {
        let mut line = Line::new();
        line.text("signal ");
        line.decimal(11);
        line.text(" at ");
        line.hex(0);
        line.text(" ");
        line.hex(usize::MAX);
        let want = format!("signal 11 at 0x0 {:#x}", usize::MAX);
        assert_eq!(&line.buf[..line.len], want.as_bytes());
        for _ in 0..40 {
            line.text("0123456789");
        }
        assert_eq!(line.len, line.buf.len());
    }
}
