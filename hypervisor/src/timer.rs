use crate::global_const::MAX_NUMBER_OF_GUESTS;

#[derive(Debug, Copy, Clone)]
pub struct VmTimers {
    timers : [VmTimer; MAX_NUMBER_OF_GUESTS]
}

impl VmTimers {
    pub fn new() -> VmTimers {
        VmTimers{
            timers: [VmTimer::new() ; MAX_NUMBER_OF_GUESTS]
        }
    }
    pub fn tick_vm_timers(&mut self, amount: usize ){
        let mut i = 0;
        while i < MAX_NUMBER_OF_GUESTS-1 {
            self.timers[i].tick(amount as u64);
            i += 1;
        }
    }
    pub fn debug_print(&self) {
        for timer in self.timers {
            log::info!("timer {}, mtime value: {}, mtimecmp value: {}", timer.enabled, timer.mtime, timer.mtimecmp);
        }
    }
    pub fn check_timers(&self) -> [bool; MAX_NUMBER_OF_GUESTS] {
        let mut vm_timer_list = [false ; MAX_NUMBER_OF_GUESTS];
        let mut i = 0;
        while i < MAX_NUMBER_OF_GUESTS-1 {
            let vmtimer = self.timers[i];
            if vmtimer.enabled {
                if vmtimer.mtime >= vmtimer.mtimecmp {
                    vm_timer_list[i] = true;
                }
            }
            i += 1;
        }
        return vm_timer_list
    }
}

#[derive(Debug, Copy, Clone)]
pub struct VmTimer {
    enabled: bool,
    mtime: u64,
    mtimecmp: u64
}

impl VmTimer {
    pub fn new() -> VmTimer {
        VmTimer{
            enabled: false,
            mtime: 0,
            mtimecmp: 0
        }
    }

    pub fn tick(&mut self, amount: u64){
        if self.enabled {
            self.mtime += amount;
        }
    }

    pub fn set_timer(&mut self, amount: u64){
        self.enabled = true;
        self.mtimecmp = amount;
        self.mtime = 0;
    }
}

lazy_static::lazy_static! {
    pub static ref TIMERS: spin::Mutex<VmTimers> = spin::Mutex::new(VmTimers::new());
}

pub struct TimerHandle {
    guest_number: u64,
}

impl TimerHandle {
    pub fn new(guest_number: u64) -> Self {
        Self { guest_number }
    }
}

impl rustsbi::Timer for TimerHandle {
    #[inline]
    fn set_timer(&self, stime_value: u64) {
        let mut timer = TIMERS.lock();
        timer.timers[self.guest_number as usize].set_timer(stime_value);
        crate::riscv::csr::hvip::clear_timing_interrupt();
        log::info!("Setting timer mtimecmp {} for guest {}", stime_value, self.guest_number);
    }
}
