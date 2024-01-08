global_asm!(include_str!("hypervisor.S"));

use crate::guest::Guest;
use crate::memlayout;
use crate::paging;
use crate::plic;
use crate::clint;
use crate::riscv;
use crate::riscv::gpr::Register;
use crate::sbi::VmSBI;
use crate::timer::VmTimers;
use crate::uart;
use crate::virtio;
use crate::sbi;
use crate::global_const::{HYPERVISOR_TIMER_TICK, MAX_NUMBER_OF_GUESTS};
use core::arch::asm;
use core::arch::global_asm;
use core::convert::TryFrom;
use core::fmt::Error;

use rustsbi::{RustSBI, spec::binary::Error as SbiError};

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;

extern "C" {
    #[link_name = "hypervisor_entrypoint"]
    pub fn entrypoint();

    #[link_name = "trap_to_hypervisor"]
    pub fn trap();
}

struct hypervisor {
    hypervisor_guests : [Guest ; MAX_NUMBER_OF_GUESTS]
}

#[no_mangle]
pub fn rust_hypervisor_entrypoint() -> ! {
    log::info!("hypervisor started");

    if let Err(e) = riscv::interrupt::free(|_| init()) {
        panic!("Failed to init hypervisor. {:?}", e)
    }
    log::info!("succeeded in initializing hypervisor");

    // TODO (enhnancement): multiplex here
    let guest_name = "guest01";
    log::info!("a new guest instance: {}", guest_name);
    log::info!("-> create metadata set");
    let mut guest = riscv::interrupt::free(|_| Guest::new(guest_name));
    log::info!("-> load a tiny kernel image");
    riscv::interrupt::free(|_| guest.load_from_disk());

    log::info!("switch to guest");
    switch_to_guest(&guest);
}

pub fn init() -> Result<(), Error> {
    // inti memory allocator
    paging::init();

    // init virtio
    virtio::init();

    // hedeleg: delegate some synchoronous exceptions
    riscv::csr::hedeleg::write(riscv::csr::hedeleg::INST_ADDR_MISALIGN 
                            | riscv::csr::hedeleg::BREAKPOINT 
                            | riscv::csr::hedeleg::ENV_CALL_FROM_U_MODE_OR_VU_MODE 
                            | riscv::csr::hedeleg::INST_PAGE_FAULT 
                            | riscv::csr::hedeleg::LOAD_PAGE_FAULT 
                            | riscv::csr::hedeleg::STORE_AMO_PAGE_FAULT);

    // hideleg: delegate all interrupts
    riscv::csr::hideleg::write(
        riscv::csr::hideleg::VSEIP | riscv::csr::hideleg::VSTIP | riscv::csr::hideleg::VSSIP,
    );

    // hvip: clear all interrupts first
    riscv::csr::hvip::write(0);

    // stvec: set handler
    riscv::csr::stvec::set(&(trap as unsafe extern "C" fn()));
    assert_eq!(
        riscv::csr::stvec::read(),
        (trap as unsafe extern "C" fn()) as usize
    );

    // allocate memory region for TrapFrame and set it sscratch
    let trap_frame = paging::alloc();
    riscv::csr::sscratch::write(trap_frame.address().to_usize());
    log::info!("sscratch: {:016x}", riscv::csr::sscratch::read());

    // enable interupts
    enable_interrupt();

    // TODO: hip and sip
    // TODO: hie and sie

    // leave
    Ok(())
}

fn enable_interrupt() {
    // TODO (enhancement): UART0

    // configure PLIC
    plic::enable_interrupt();

    // sie; enable external interrupt
    // TODO (enhancement): timer interrupt
    riscv::csr::sie::enable_hardware_timer();

    // TODO (enhancement): software interrupt
    let current_sie = riscv::csr::sie::read();
    riscv::csr::sie::write(current_sie | (riscv::csr::sie::SEIE as usize));

    // sstatus: enable global interrupt
    riscv::csr::sstatus::set_sie(true);
}

pub fn switch_to_guest(target: &Guest) -> ! {
    // hgatp: set page table for guest physical address translation
    riscv::csr::hgatp::set(&target.hgatp);
    riscv::instruction::hfence_gvma();
    assert_eq!(target.hgatp.to_usize(), riscv::csr::hgatp::read());

    // hstatus: handle SPV change the virtualization mode to 0 after sret
    riscv::csr::hstatus::set_spv(riscv::csr::VirtualzationMode::Guest);

    // sstatus: handle SPP to 1 to change the privilege level to S-Mode after sret
    riscv::csr::sstatus::set_spp(riscv::csr::CpuMode::S);

    // sepc: set the addr to jump
    riscv::csr::sepc::set(&target.sepc);

    // jump!
    riscv::instruction::sret();
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TrapFrame {
    pub regs: [usize; 32],  // 0 - 255
    pub fregs: [usize; 32], // 256 - 511
    pub pc: usize,          // 512
}

fn show_trapinfo(
    sepc: usize,           // a0
    stval: usize,          // a1
    scause: usize,         // a2
    sstatus: usize,        // a3
    frame: *mut TrapFrame, // a4
){
    log::info!("<--------- trap --------->");
    log::info!("sepc: 0x{:016x}", sepc,);
    log::info!("stval: 0x{:016x}", stval,);
    log::info!("scause: 0x{:016x}", scause,);
    log::info!("sstatus: 0x{:016x}", sstatus,);

    log::info!("------- trapframe --------");
    let user_frame = unsafe{*frame.clone()};
    let mut i = 0;
    for reg in user_frame.regs {
        let reg_name = Register::try_from(i).unwrap();
        print!("{:<3} = 0x{:016x} ", reg_name, reg);
		if i % 4 == 3 {
			println!();
		} else {
			print!("| ")
		}
        i += 1;
    }
    log::info!("------- registers --------");
    riscv::gpr::dump();
    log::info!("---------  S csr ---------");
    riscv::csr::dump_s_csr();
    log::info!("---------  H csr ---------");
    riscv::csr::dump_h_csr();
    log::info!("--------- VS csr ---------");
    riscv::csr::dump_vs_csr();
    log::info!("-------- Prev Mode -------");
    let prev = riscv::csr::hstatus::previous_mode().unwrap();
    let mode_str = match prev {
        riscv::csr::PreviousMode::U_mode  => "User mode (U)",
        riscv::csr::PreviousMode::HS_mode => "Hypervisor mode (HS)",
        riscv::csr::PreviousMode::M_mode  => "Machine Mode (M)",
        riscv::csr::PreviousMode::VU_mode => "Virtual User Mode (VU)",
        riscv::csr::PreviousMode::VS_mode => "Virtual Supervisor Mode (VS)",
    };
    log::info!("Previous Mode before trap: {}", mode_str);
}

#[no_mangle]
pub extern "C" fn rust_strap_handler(
    sepc: usize,           // a0
    stval: usize,          // a1
    scause: usize,         // a2
    sstatus: usize,        // a3
    frame: *mut TrapFrame, // a4
) -> usize {
    log::debug!("<--------- trap --------->");
    log::debug!("sepc: 0x{:016x}", sepc,);
    log::debug!("stval: 0x{:016x}", stval,);
    log::debug!("scause: 0x{:016x}", scause,);
    log::debug!("sstatus: 0x{:016x}", sstatus,);

    let is_async = scause >> 63 & 1 == 1;
    let cause_code = scause & 0xfff;
    if is_async {
        match cause_code {
            // external interrupt
            9 => {
                if let Some(interrupt) = plic::get_claim() {
                    log::debug!("interrupt id: {}", interrupt);
                    match interrupt {
                        1..=8 => {
                            virtio::handle_interrupt(interrupt);
                        }
                        10 => {
                            uart::handle_interrupt();
                        }
                        _ => {
                            unimplemented!()
                        }
                    }
                    plic::complete(interrupt);
                } else {
                    panic!("invalid state")
                }
            }
            5 => {
                //timer interrupt
                //show_trapinfo(sepc,stval,scause,sstatus,frame);
                log::debug!("Hypervisor timer interrupt fired");
                riscv::csr::sip::clear_stimer();
                riscv::csr::sie::clear_hardware_timer();
                assert_eq!(
                    riscv::csr::sie::read() >> 5 & 0b1,
                    0
                );

                if let Some(mut timer) = crate::timer::TIMERS.try_lock() {
                    //let mut timer = timer::TIMER.lock();
                    timer.tick_vm_timers(HYPERVISOR_TIMER_TICK);
                    let timer_trigger_list = timer.check_timers();
                    //println!("{:?}", timer_trigger_list);
                    //timer.debug_print();

                    // TODO: loop through all avalible guests
                    // assuming 0 now since we have hardcoded one vm
                    let guest0_timer_intr_trigger = timer_trigger_list [0];
                    if guest0_timer_intr_trigger {
                        log::info!("triggering timer interrupt on guest0");
                        riscv::csr::hvip::trigger_timing_interrupt();
                    }
                    
                } 
            }
            // timer interrupt & software interrrupt
            _ => {
                unimplemented!("Unknown interrupt id: {}", cause_code);
            }
        }
    } else {
        match cause_code {
            8 => {
                log::info!("environment call from U-mode / VU-mode at 0x{:016x}", sepc);
                // TODO: better handling
                loop {}
            }
            10 => {
                log::info!("environment call from VS-mode at 0x{:016x}", sepc);
                let user_frame = unsafe{*frame.clone()};
                //println!("{:?}", user_frame);
                
                // Hardcoded for now
                let guest_number = 0;

                let a7 = user_frame.regs[17];
                let a6 = user_frame.regs[16];
                let a1 = user_frame.regs[11];
                let a0 = user_frame.regs[10];
                let params = [user_frame.regs[10], user_frame.regs[11], user_frame.regs[12], user_frame.regs[13], user_frame.regs[14], user_frame.regs[15]];
                log::info!("a0: 0x{:x}, a1: 0x{:x}, a6: 0x{:x}, a7: 0x{:x}", a0, a1, a6, a7);
                let sbi = VmSBI::with_guest_number(guest_number);
                let sbi_result = sbi.handle_ecall(a7, a6, params);
                match sbi_result.into_result() {
                    Ok(_) =>                            log::info!("SBI result SBI_SUCCESS            "),
                    Err(SbiError::NotSupported) =>      log::info!("SBI result SBI_ERR_NOT_SUPPORTED  "),
                    Err(SbiError::InvalidParam) =>      log::info!("SBI result SBI_ERR_INVALID_PARAM  "),
                    Err(SbiError::InvalidAddress) =>    log::info!("SBI result SBI_ERR_INVALID_ADDRESS"),
                    Err(SbiError::Failed) =>            log::info!("SBI result SBI_ERR_FAILED         "),
                    _ =>                                log::info!("SBI result Error {:?}", sbi_result)
                }
                log::info!("SBI result {:?}", sbi_result);
                // Maybe there is a better way todo this
                unsafe {
                    (*frame).regs[10] = sbi_result.error;
                    (*frame).regs[11] = sbi_result.value;
                }
                return sepc + 0x4; // Skips to the next instruction in guest
                //loop {}
            }
            21 => {
                show_trapinfo(sepc,stval,scause,sstatus,frame);
                log::info!("exception: load guest page fault at 0x{:016x}", sepc);
                // TODO (enhancement): demand paging
                loop {}
            }
            23 => {
                show_trapinfo(sepc,stval,scause,sstatus,frame);
                log::info!("exception: store/amo guest-page fault at 0x{:016x}", sepc);
                // TODO: better handling
                loop {}
            }
            _ => {
                show_trapinfo(sepc,stval,scause,sstatus,frame);
                unimplemented!("Unknown Exception id: {}", cause_code);
            }
        }
    }
    sepc
}
