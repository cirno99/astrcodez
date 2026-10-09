//! astrcode 桌面应用入口。

#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

fn main() {
    #[cfg(target_os = "linux")]
    disable_transparent_huge_pages();
    #[cfg(not(target_env = "msvc"))]
    tune_jemalloc_decay();
    astrcode_gui::run();
}

/// 流式输出这类突发分配后的脏页归还节奏：1s 内还给 OS，muzzy 立即归还。
///
/// jemalloc 默认 dirty/muzzy decay 都是 10s，桌面 App 常驻，RSS 观感优先于
/// 轻微的缺页开销。decay_ms 是运行时可调的 mallctl，放在入口最先执行。
#[cfg(not(target_env = "msvc"))]
fn tune_jemalloc_decay() {
    const DIRTY_DECAY_MS: i64 = 1_000;
    const MUZZY_DECAY_MS: i64 = 0;

    // SAFETY：mallctl 名称是 jemalloc 文档定义的字面量，i64 与 ssize_t 匹配。
    unsafe {
        // `arenas.*` 是新建 arena 的默认值；已存在的 arena 逐个补上（见循环内注释）。
        if let Err(error) =
            tikv_jemalloc_ctl::raw::update(b"arenas.dirty_decay_ms\0", DIRTY_DECAY_MS)
        {
            eprintln!("jemalloc arenas.dirty_decay_ms 调整失败：{error}");
        }
        if let Err(error) =
            tikv_jemalloc_ctl::raw::update(b"arenas.muzzy_decay_ms\0", MUZZY_DECAY_MS)
        {
            eprintln!("jemalloc arenas.muzzy_decay_ms 调整失败：{error}");
        }
        let narenas: u32 = match tikv_jemalloc_ctl::raw::read(b"arenas.narenas\0") {
            Ok(narenas) => narenas,
            Err(error) => {
                eprintln!("jemalloc arenas.narenas 读取失败：{error}");
                return;
            },
        };
        for arena in 0..narenas {
            let mut dirty = format!("arena.{arena}.dirty_decay_ms");
            dirty.push('\0');
            let mut muzzy = format!("arena.{arena}.muzzy_decay_ms");
            muzzy.push('\0');
            // 尚未懒创建的 arena 读/写都报 EFAULT；它们创建时继承上面的新默认值，
            // 因此只补写读得通的（已经存在的）arena。
            if tikv_jemalloc_ctl::raw::read::<i64>(dirty.as_bytes()).is_err() {
                continue;
            }
            if let Err(error) = tikv_jemalloc_ctl::raw::update(dirty.as_bytes(), DIRTY_DECAY_MS) {
                eprintln!("jemalloc {dirty} 调整失败：{error}");
            }
            if let Err(error) = tikv_jemalloc_ctl::raw::update(muzzy.as_bytes(), MUZZY_DECAY_MS) {
                eprintln!("jemalloc {muzzy} 调整失败：{error}");
            }
        }
    }
}

/// 关闭本进程的透明大页（THP）。
///
/// 本进程的堆是稀疏写入的：jemalloc 的 extent 分散在十几个 arena 里，元数据与小
/// 对象散布其间。系统 `transparent_hugepage/enabled=always` 时，2MB 对齐区段里的
/// 任意一次写故障都会落地一整页 2MB 的零填充大页并计入 RssAnon，于是常驻内存里
/// 绝大部分是零字节。实测同一份二进制、同一份数据：开启 THP 时 RssAnon 188MB
/// （其中 184MB 是大页，非零内容仅约 3MB），关闭后 19MB，线程数与功能不变。
///
/// 桌面 App 常驻，RSS 观感优先于 TLB 收益：堆本身稀疏，THP 几乎没有可覆盖的连续
/// 热点。放在入口最先执行，让后续映射都不参与大页。
#[cfg(target_os = "linux")]
fn disable_transparent_huge_pages() {
    // SAFETY：PR_SET_THP_DISABLE 只把第二个参数当布尔标志读，不接触用户内存。
    if unsafe { libc::prctl(libc::PR_SET_THP_DISABLE, 1, 0, 0, 0) } != 0 {
        eprintln!("关闭透明大页失败：{}", std::io::Error::last_os_error());
    }
}

#[cfg(all(test, not(target_env = "msvc")))]
mod tests {
    use tikv_jemalloc_ctl::raw;

    #[test]
    fn decay_tuning_takes_effect() {
        super::tune_jemalloc_decay();
        // SAFETY：mallctl 名称是 jemalloc 文档定义的字面量。
        unsafe {
            // 只有测试可以 unwrap：读不回来说明 mallctl 桥或名字又错了，必须炸出来。
            let dirty: i64 = raw::read(b"arenas.dirty_decay_ms\0").unwrap();
            assert_eq!(dirty, 1_000);
            let muzzy: i64 = raw::read(b"arenas.muzzy_decay_ms\0").unwrap();
            assert_eq!(muzzy, 0);
            let narenas: u32 = raw::read(b"arenas.narenas\0").unwrap();
            assert!(narenas > 0);
            let arena0_dirty: i64 = raw::read(b"arena.0.dirty_decay_ms\0").unwrap();
            assert_eq!(arena0_dirty, 1_000);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn transparent_huge_pages_are_disabled() {
        super::disable_transparent_huge_pages();
        // SAFETY：PR_GET_THP_DISABLE 不触碰用户内存，返回当前进程的该标志位。
        let disabled = unsafe { libc::prctl(libc::PR_GET_THP_DISABLE, 0, 0, 0, 0) };
        assert_eq!(disabled, 1);
    }
}
