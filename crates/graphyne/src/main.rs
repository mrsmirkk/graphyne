// Release builds are GUI-subsystem apps on Windows (no console window flashes up).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(feature = "alloc-stats")]
    alloc_stats::start_reporter();
    graphyne_ui::run();
}

/// `--features alloc-stats`: count live heap bytes and print them every 5 s to stderr,
/// to tell Graphyne's own memory apart from GPU-driver and OS allocations.
#[cfg(feature = "alloc-stats")]
mod alloc_stats {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting;
    static LIVE: AtomicUsize = AtomicUsize::new(0);
    static PEAK: AtomicUsize = AtomicUsize::new(0);

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let p = unsafe { System.alloc(layout) };
            if !p.is_null() {
                let now = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
                PEAK.fetch_max(now, Ordering::Relaxed);
            }
            p
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) };
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let p = unsafe { System.realloc(ptr, layout, new_size) };
            if !p.is_null() {
                if new_size >= layout.size() {
                    let now = LIVE.fetch_add(new_size - layout.size(), Ordering::Relaxed) + new_size - layout.size();
                    PEAK.fetch_max(now, Ordering::Relaxed);
                } else {
                    LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
                }
            }
            p
        }
    }

    #[global_allocator]
    static ALLOC: Counting = Counting;

    pub fn start_reporter() {
        std::thread::spawn(|| loop {
            std::thread::sleep(std::time::Duration::from_secs(5));
            let mb = |b: usize| b as f64 / (1024.0 * 1024.0);
            eprintln!("rust heap: live {:.1} MB, peak {:.1} MB", mb(LIVE.load(Ordering::Relaxed)), mb(PEAK.load(Ordering::Relaxed)));
        });
    }
}
