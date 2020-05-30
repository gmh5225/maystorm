// Thread Scheduler

use super::arch::cpu::Cpu;
use crate::myos::io::graphics::*;
use crate::myos::mem::alloc::*;
use crate::myos::sync::spinlock::*;
use crate::myos::system::*;
use crate::*;
use alloc::boxed::Box;
use alloc::vec::*;
use core::num::*;
use core::ops::*;
use core::ptr::*;
use core::sync::atomic::*;
// use bitflags::*;

static mut TIMER_SOURCE: Option<Box<dyn TimerSource>> = None;

extern "C" {
    fn sch_switch_context(current: *mut u8, next: *mut u8);
    fn sch_make_new_thread(context: *mut u8, new_sp: *mut c_void, start: usize, args: *mut c_void);
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct ThreadId(pub usize);

unsafe impl Sync for ThreadId {}

unsafe impl Send for ThreadId {}

impl ThreadId {
    pub const fn as_u64(&self) -> u64 {
        self.0 as u64
    }
}

pub trait TimerSource {
    fn create(&self, h: TimeMeasure) -> TimeMeasure;
    fn until(&self, h: TimeMeasure) -> bool;
    fn diff(&self, h: TimeMeasure) -> isize;
}

#[derive(Debug, Copy, Clone)]
pub struct Timer {
    deadline: TimeMeasure,
}

impl Timer {
    pub const NULL: Timer = Timer {
        deadline: TimeMeasure::NULL,
    };

    pub fn new(duration: TimeMeasure) -> Self {
        let timer = unsafe { TIMER_SOURCE.as_ref().unwrap() };
        Timer {
            deadline: timer.create(duration),
        }
    }

    #[must_use]
    pub fn until(&self) -> bool {
        match self.deadline {
            TimeMeasure::NULL => false,
            TimeMeasure::FOREVER => true,
            _ => {
                let timer = unsafe { TIMER_SOURCE.as_ref().unwrap() };
                timer.until(self.deadline)
            }
        }
    }

    pub(crate) unsafe fn set_timer(source: Box<dyn TimerSource>) {
        TIMER_SOURCE = Some(source);
    }

    pub fn sleep(duration: TimeMeasure) {
        if GlobalScheduler::is_enabled() {
            GlobalScheduler::wait_for(None, duration);
        } else {
            let timer = unsafe { TIMER_SOURCE.as_ref().unwrap() };
            let deadline = timer.create(duration);
            while timer.until(deadline) {
                unsafe {
                    Cpu::halt();
                }
            }
        }
    }

    pub fn usleep(us: u64) {
        Self::sleep(TimeMeasure::from_micros(us));
    }
}

#[repr(transparent)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd)]
pub struct TimeMeasure(pub i64);

impl TimeMeasure {
    pub const NULL: TimeMeasure = TimeMeasure(0);
    pub const FOREVER: TimeMeasure = TimeMeasure(i64::MAX);

    pub fn is_null(&self) -> bool {
        *self == Self::NULL
    }

    pub fn is_forever(&self) -> bool {
        *self == Self::FOREVER
    }

    pub const fn from_micros(us: u64) -> Self {
        TimeMeasure(us as i64)
    }

    pub const fn from_millis(ms: u64) -> Self {
        TimeMeasure(ms as i64 * 1000)
    }

    pub const fn from_secs(s: u64) -> Self {
        TimeMeasure(s as i64 * 1000_000)
    }

    pub const fn as_micros(self) -> i64 {
        self.0 as i64
    }

    pub const fn as_millis(self) -> i64 {
        self.0 as i64 / 1000
    }

    pub const fn as_secs(self) -> i64 {
        self.0 as i64 / 1000_000
    }
}

impl Add<isize> for TimeMeasure {
    type Output = Self;
    fn add(self, rhs: isize) -> Self {
        Self(self.0 + rhs as i64)
    }
}

impl Sub<isize> for TimeMeasure {
    type Output = Self;
    fn sub(self, rhs: isize) -> Self {
        Self(self.0 - rhs as i64)
    }
}

static mut GLOBAL_SCHEDULER: GlobalScheduler = GlobalScheduler::new();
const SIZE_OF_URGENT_QUEUE: usize = 512;
const SIZE_OF_MAIN_QUEUE: usize = 512;

/// System Global Scheduler
pub struct GlobalScheduler {
    next_thread_id: AtomicUsize,
    urgent: Option<Box<ThreadQueue>>,
    ready: Option<Box<ThreadQueue>>,
    retired: Option<Box<ThreadQueue>>,
    locals: Vec<Box<LocalScheduler>>,
    is_enabled: AtomicBool,
}

impl GlobalScheduler {
    const fn new() -> Self {
        Self {
            next_thread_id: AtomicUsize::new(0),
            urgent: None,
            ready: None,
            retired: None,
            locals: Vec::new(),
            is_enabled: AtomicBool::new(false),
        }
    }

    pub(crate) fn start(system: &System, f: fn(*mut c_void) -> (), args: *mut c_void) -> ! {
        let sch = unsafe { &mut GLOBAL_SCHEDULER };

        sch.urgent = Some(ThreadQueue::with_capacity(SIZE_OF_URGENT_QUEUE));
        sch.ready = Some(ThreadQueue::with_capacity(SIZE_OF_MAIN_QUEUE));
        sch.retired = Some(ThreadQueue::with_capacity(SIZE_OF_MAIN_QUEUE));

        for index in 0..system.number_of_active_cpus() {
            sch.locals.push(LocalScheduler::new(ProcessorIndex(index)));
        }

        for _ in 0..30 {
            Self::retire(NativeThread::new(
                Priority::Normal,
                Some(Self::scheduler_thread),
                null_mut(),
            ));
        }

        Self::retire(NativeThread::new(Priority::Normal, Some(f), args));

        sch.is_enabled.store(true, Ordering::Relaxed);

        // loop {
        //     GlobalScheduler::wait_for(None, TimeMeasure(0));
        // }
        loop {
            unsafe {
                Cpu::halt();
            }
        }
    }

    fn next_thread_id() -> ThreadId {
        let sch = unsafe { &GLOBAL_SCHEDULER };
        ThreadId(sch.next_thread_id.fetch_add(1, Ordering::Relaxed))
    }

    // Perform a Preemption
    pub(crate) fn reschedule() {
        unsafe {
            llvm_asm!("mov $$0xdeadbeef, %r8d":::"r8":"volatile");
        }
        if Self::is_enabled() {
            Cpu::without_interrupts(|| {
                let lsch = Self::local_scheduler();
                if lsch.current.borrow().priority != Priority::Realtime {
                    if lsch.current.using(|current| current.quantum.consume()) {
                        LocalScheduler::next_thread(lsch);
                    }
                }
            })
        }
    }

    /// Wait for Event or Timer
    pub fn wait_for(object: Option<&SignallingObject>, duration: TimeMeasure) {
        Cpu::without_interrupts(|| {
            let lsch = Self::local_scheduler();
            if let Some(object) = object {
                object.set(lsch.current_thread()).unwrap();
            }
            lsch.current.using(|current| {
                current.deadline = Timer::new(duration);
            });
            LocalScheduler::next_thread(lsch);
        });
    }

    pub fn signal(object: &SignallingObject) {
        object.unbox();
    }

    fn local_scheduler() -> &'static mut LocalScheduler {
        let sch = unsafe { &mut GLOBAL_SCHEDULER };
        let cpu_index = Cpu::current_processor_index().unwrap();
        sch.locals.get_mut(cpu_index.0).unwrap()
    }

    // Get Next Thread from queue
    fn next() -> Option<ThreadHandle> {
        let sch = unsafe { &mut GLOBAL_SCHEDULER };
        if let Some(next) = sch.urgent.as_mut().unwrap().read() {
            return Some(next);
        }
        while let Some(next) = sch.ready.as_mut().unwrap().read() {
            if next.borrow().deadline.until() {
                GlobalScheduler::retire(next);
                continue;
            } else {
                return Some(next);
            }
        }
        let front = sch.ready.as_mut().unwrap();
        let back = sch.retired.as_mut().unwrap();
        loop {
            if let Some(retired) = back.read() {
                front.write(retired).unwrap();
            } else {
                return None;
            }
        }
    }

    // Retire Thread
    fn retire(thread: ThreadHandle) {
        let sch = unsafe { &mut GLOBAL_SCHEDULER };
        let priority = thread.borrow().priority;
        if priority != Priority::Idle {
            sch.retired.as_mut().unwrap().write(thread).unwrap();
        }
    }

    fn scheduler_thread(_args: *mut c_void) {
        // TODO:
        let id = NativeThread::current_id().0 as isize;
        let mut counter: usize = 0;
        loop {
            counter += 0x040506;
            stdout()
                .fb()
                .fill_rect(Rect::new(10 * id, 5, 8, 8), Color::from(counter as u32));
            Self::wait_for(None, TimeMeasure::from_millis(5));
        }
    }

    pub fn print_statistics() {
        let sch = unsafe { &GLOBAL_SCHEDULER };
        for lsch in &sch.locals {
            println!("{} = {}", lsch.index.0, lsch.count.load(Ordering::Relaxed));
        }
    }

    pub fn is_enabled() -> bool {
        let sch = unsafe { &GLOBAL_SCHEDULER };
        sch.is_enabled.load(Ordering::Relaxed)
    }
}

// Processor Local Scheduler
struct LocalScheduler {
    pub index: ProcessorIndex,
    count: AtomicUsize,
    idle: ThreadHandle,
    current: ThreadHandle,
    retired: Option<ThreadHandle>,
}

impl LocalScheduler {
    fn new(index: ProcessorIndex) -> Box<Self> {
        let idle = NativeThread::new(Priority::Idle, None, null_mut());
        Box::new(Self {
            index: index,
            count: AtomicUsize::new(0),
            idle: idle,
            current: idle,
            retired: None,
        })
    }

    fn next_thread(lsch: &'static mut Self) {
        Cpu::check_irq().ok().unwrap();
        let current = lsch.current;
        let next = match GlobalScheduler::next() {
            Some(next) => next,
            None => lsch.idle,
        };
        if current.borrow().id == next.borrow().id {
            // TODO: adjust statistics
        } else {
            lsch.count.fetch_add(1, Ordering::Relaxed);
            lsch.retired = Some(current);
            lsch.current = next;
            current.using(|thread| thread.quantum = thread.default_quantum);
            unsafe {
                sch_switch_context(
                    &current.borrow().context as *const u8 as *mut u8,
                    &next.borrow().context as *const u8 as *mut u8,
                );
            }
            let lsch = GlobalScheduler::local_scheduler();
            let current = lsch.current;
            current.using(|thread| thread.deadline = Timer::NULL);
            let retired = lsch.retired.unwrap();
            // if let Some(retired) = lsch.retired {
            lsch.retired = None;
            GlobalScheduler::retire(retired);
            // }
        }
    }

    fn current_thread(&self) -> ThreadHandle {
        self.current
    }
}

#[no_mangle]
pub extern "C" fn sch_setup_new_thread() {
    let lsch = GlobalScheduler::local_scheduler();
    if let Some(retired) = lsch.retired {
        lsch.retired = None;
        GlobalScheduler::retire(retired);
    }
}

#[non_exhaustive]
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd)]
pub enum Priority {
    Idle = 0,
    Low,
    Normal,
    High,
    Realtime,
}

impl Priority {
    pub fn useful(&self) -> bool {
        match *self {
            Priority::Idle => false,
            _ => true,
        }
    }
}

#[repr(transparent)]
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd)]
pub struct Quantum(pub u8);

unsafe impl Sync for Quantum {}

impl Quantum {
    fn consume(&mut self) -> bool {
        if self.0 > 1 {
            self.0 -= 1;
            false
        } else {
            true
        }
    }
}

impl From<Priority> for Quantum {
    fn from(priority: Priority) -> Self {
        match priority {
            Priority::High => Quantum(25),
            Priority::Normal => Quantum(10),
            Priority::Low => Quantum(5),
            _ => Quantum(1),
        }
    }
}

static mut THREAD_POOL: ThreadPool = ThreadPool::new();

struct ThreadPool {
    vec: Vec<Box<NativeThread>>,
    lock: Spinlock,
}

impl ThreadPool {
    const fn new() -> Self {
        Self {
            vec: Vec::new(),
            lock: Spinlock::new(),
        }
    }

    fn add(thread: Box<NativeThread>) -> ThreadHandle {
        let shared = unsafe { &mut THREAD_POOL };
        shared.lock.lock();
        shared.vec.push(thread);
        let len = shared.vec.len();
        shared.lock.unlock();
        ThreadHandle::new(len).unwrap()
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct ThreadHandle(NonZeroUsize);

impl ThreadHandle {
    pub fn new(val: usize) -> Option<Self> {
        NonZeroUsize::new(val).map(|x| Self(x))
    }

    pub const fn as_usize(&self) -> usize {
        self.0.get()
    }

    const fn as_index(&self) -> usize {
        self.as_usize() - 1
    }

    #[inline]
    fn using<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut NativeThread) -> R,
    {
        let shared = unsafe { &mut THREAD_POOL };
        let thread = shared.vec[self.as_index()].as_mut();
        f(thread)
    }

    fn borrow(&self) -> &'static NativeThread {
        let shared = unsafe { &THREAD_POOL };
        shared.vec[self.as_index()].as_ref()
    }
}

#[derive(Debug)]
pub struct SignallingObject(AtomicUsize);

unsafe impl Sync for SignallingObject {}

unsafe impl Send for SignallingObject {}

impl SignallingObject {
    const NULL: usize = 0;

    pub fn new() -> Box<Self> {
        Box::new(Self(AtomicUsize::new(Self::NULL)))
    }

    pub fn set(&self, value: ThreadHandle) -> Result<(), ()> {
        let value = value.as_usize();
        match self
            .0
            .compare_exchange(Self::NULL, value, Ordering::SeqCst, Ordering::Relaxed)
        {
            Ok(_) => Ok(()),
            Err(_) => Err(()),
        }
    }

    pub fn unbox(&self) -> Option<ThreadHandle> {
        ThreadHandle::new(self.0.swap(Self::NULL, Ordering::SeqCst))
    }

    pub fn wait(&self, duration: TimeMeasure) {
        GlobalScheduler::wait_for(Some(self), duration)
    }

    pub fn signal(&self) {
        GlobalScheduler::signal(&self)
    }
}

const SIZE_OF_CONTEXT: usize = 512;
const SIZE_OF_STACK: usize = 0x10000;

type ThreadStart = fn(*mut c_void) -> ();

#[allow(dead_code)]
pub(crate) struct NativeThread {
    context: [u8; SIZE_OF_CONTEXT],
    id: ThreadId,
    priority: Priority,
    quantum: Quantum,
    default_quantum: Quantum,
    deadline: Timer,
    // attributes: ThreadFlags,
    //name: [],
}

unsafe impl Sync for NativeThread {}

impl NativeThread {
    fn new(priority: Priority, start: Option<ThreadStart>, args: *mut c_void) -> ThreadHandle {
        let quantum = Quantum::from(priority);
        let handle = ThreadPool::add(Box::new(Self {
            context: [0; SIZE_OF_CONTEXT],
            id: GlobalScheduler::next_thread_id(),
            priority: priority,
            quantum: quantum,
            default_quantum: quantum,
            deadline: Timer::NULL,
        }));
        if let Some(start) = start {
            handle.using(|thread| unsafe {
                let stack = CustomAlloc::zalloc(SIZE_OF_STACK).unwrap().as_ptr();
                sch_make_new_thread(
                    thread.context.as_mut_ptr(),
                    stack.add(SIZE_OF_STACK),
                    start as usize,
                    args,
                );
            })
        }
        handle
    }

    pub fn current_id() -> ThreadId {
        GlobalScheduler::local_scheduler().current.borrow().id
    }

    pub fn current() -> ThreadHandle {
        GlobalScheduler::local_scheduler().current_thread()
    }

    pub fn exit(_exit_code: usize) -> ! {
        panic!("NO MORE THREAD!!!");
    }
}

#[non_exhaustive]
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd)]
pub enum Irql {
    Passive = 0,
    Dispatch,
    Device,
    High,
}

impl Irql {
    pub fn raise(new_irql: Irql) -> Result<Irql, ()> {
        let old_irql = Self::current();
        if old_irql > new_irql {
            panic!("IRQL_NOT_LESS_OR_EQUAL");
        }
        Ok(old_irql)
    }

    pub fn lower(_new_irql: Irql) -> Result<(), ()> {
        Ok(())
    }

    pub fn current() -> Irql {
        Irql::Passive
    }
}

struct ThreadQueue {
    read: AtomicUsize,
    write: AtomicUsize,
    mask: usize,
    lock: Spinlock,
    buf: Vec<AtomicUsize>,
    // TODO: lock-free
}

unsafe impl Sync for ThreadQueue {}

impl ThreadQueue {
    const NULL: usize = 0;

    fn with_capacity(capacity: usize) -> Box<Self> {
        assert_eq!(capacity.count_ones(), 1);
        let mask = capacity - 1;
        let mut buf = Vec::<AtomicUsize>::with_capacity(capacity);
        for _ in 0..capacity {
            buf.push(AtomicUsize::new(Self::NULL));
        }
        Box::new(Self {
            read: AtomicUsize::new(0),
            write: AtomicUsize::new(0),
            mask: mask,
            lock: Spinlock::new(),
            buf: buf,
        })
    }

    fn read(&self) -> Option<ThreadHandle> {
        self.lock.synchronized(|| {
            let mask = self.mask;
            if (mask & (self.write.load(Ordering::Relaxed)))
                != (mask & (self.read.load(Ordering::Relaxed)))
            {
                let read = self.read.load(Ordering::Acquire);
                let result = self.buf[read & mask].swap(Self::NULL, Ordering::SeqCst);
                self.read.fetch_add(1, Ordering::Release);
                return ThreadHandle::new(result);
            } else {
                None
            }
        })
    }

    fn write(&self, data: ThreadHandle) -> Result<(), ()> {
        let data = data.as_usize();
        self.lock.synchronized(|| {
            let mask = self.mask;
            if (mask & (self.write.load(Ordering::Relaxed) + 1))
                != (mask & (self.read.load(Ordering::Relaxed)))
            {
                let write = mask & self.write.load(Ordering::Acquire);
                let success = self.buf[write & mask]
                    .compare_exchange(Self::NULL, data, Ordering::SeqCst, Ordering::Relaxed)
                    .is_ok();
                if success {
                    self.write.fetch_add(1, Ordering::Release);
                    return Ok(());
                } else {
                    // TODO: Inconsistency Error
                }
            }
            Err(())
        })
    }
}
