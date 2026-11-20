use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// ref: drivers/gpu/drm/amd/amdkfd/kfd_process.c
// ref: drivers/gpu/drm/amd/amdkfd/kfd_topology.c

pub const KFD_SYSFS_PATH: &str = "/sys/class/kfd/kfd";

// cu_occupancy is an instantaneous value, so average it over several samples
// taken during the update interval. Reading it makes the driver query the queues of
// the process, so do not read it as often as the performance counters.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(100);

/// The KFD topology node of a GPU.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct KfdNode {
    /// The key of `proc/<pid>/stats_<gpu_id>` and `proc/<pid>/vram_<gpu_id>`.
    /// This is not the directory index of the topology node.
    pub gpu_id: u32,
    pub cu_count: u32,
}

fn parse_property(properties: &str, key: &str) -> Option<u64> {
    properties.lines().find_map(|l| {
        let mut split = l.split_whitespace();

        if split.next()? != key {
            return None;
        }

        split.next()?.parse().ok()
    })
}

impl KfdNode {
    /// Parse `gpu_id` and `properties` of a topology node.
    /// CPU nodes (`gpu_id` is 0, no SIMD) are rejected.
    pub fn from_node_files(gpu_id: &str, properties: &str) -> Option<Self> {
        let gpu_id: u32 = gpu_id.trim().parse().ok().filter(|&id| id != 0)?;
        let simd_count = parse_property(properties, "simd_count")?;
        let simd_per_cu = parse_property(properties, "simd_per_cu")?;
        let cu_count = u32::try_from(simd_count.checked_div(simd_per_cu)?).ok()?;

        if cu_count == 0 {
            return None;
        }

        Some(Self { gpu_id, cu_count })
    }

    /// Find the topology node of the GPU by the minor number of its DRM render node.
    /// `kfd_root` is usually `KFD_SYSFS_PATH`.
    pub fn find<P: AsRef<Path>>(kfd_root: P, drm_render_minor: u32) -> Option<Self> {
        let dirs = fs::read_dir(kfd_root.as_ref().join("topology/nodes")).ok()?;

        dirs.flatten().find_map(|dir_entry| {
            let path = dir_entry.path();
            let properties = fs::read_to_string(path.join("properties")).ok()?;

            if parse_property(&properties, "drm_render_minor")? != drm_render_minor as u64 {
                return None;
            }

            let gpu_id = fs::read_to_string(path.join("gpu_id")).ok()?;

            Self::from_node_files(&gpu_id, &properties)
        })
    }

    /// Convert the number of CUs in use to the percentage of all CUs (0..=100).
    pub fn cu_occupancy_percent(&self, cu_occupancy: u32) -> u32 {
        let cu_count = self.cu_count as u64;
        let cu_occupancy = (cu_occupancy as u64).min(cu_count);

        // round to the nearest
        ((cu_occupancy * 100 + cu_count / 2) / cu_count) as u32
    }
}

/// KFD usage of a process, `None` if the kernel does not provide it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, PartialOrd)]
pub struct KfdUsage {
    pub cu_occupancy: Option<u32>, // %
    pub vram_usage: Option<u64>,   // KiB
}

#[derive(Clone, Copy, Debug, Default)]
struct OccupancySamples {
    sum: u64, // CUs
    count: u64,
}

#[derive(Clone, Debug)]
pub struct KfdStat {
    pub node: KfdNode,
    root: PathBuf,
    samples: HashMap<i32, OccupancySamples>,
    last_sample: Option<Instant>,
}

impl KfdStat {
    pub fn new(node: KfdNode) -> Self {
        Self::with_root(node, KFD_SYSFS_PATH)
    }

    pub fn with_root<P: Into<PathBuf>>(node: KfdNode, root: P) -> Self {
        Self {
            node,
            root: root.into(),
            samples: HashMap::new(),
            last_sample: None,
        }
    }

    fn read_value(&self, pid: i32, file: &str) -> Option<u64> {
        let path = self.root.join(format!("proc/{pid}")).join(file);

        fs::read_to_string(path).ok()?.trim().parse().ok()
    }

    // The file is missing if the kernel or the GPU does not support cu_occupancy.
    fn read_cu_occupancy(&self, pid: i32) -> Option<u32> {
        let file = format!("stats_{}/cu_occupancy", self.node.gpu_id);

        self.read_value(pid, &file)?.try_into().ok()
    }

    // bytes
    fn read_vram(&self, pid: i32) -> Option<u64> {
        self.read_value(pid, &format!("vram_{}", self.node.gpu_id))
    }

    /// Check if enough time has passed since the last `sample`.
    pub fn is_sample_due(&self) -> bool {
        self.last_sample
            .is_none_or(|t| SAMPLE_INTERVAL <= t.elapsed())
    }

    /// Read cu_occupancy of the processes. Unreadable files add no sample.
    pub fn sample<I: IntoIterator<Item = i32>>(&mut self, pids: I) {
        self.last_sample = Some(Instant::now());

        for pid in pids {
            let Some(cu) = self.read_cu_occupancy(pid) else {
                continue;
            };
            let s = self.samples.entry(pid).or_default();

            s.sum += (cu as u64).min(self.node.cu_count as u64);
            s.count += 1;
        }
    }

    /// Average the samples of the process, and read the other values.
    pub fn usage(&self, pid: i32) -> KfdUsage {
        let cu_occupancy = self.samples.get(&pid).filter(|s| s.count != 0).map(|s| {
            let cu_count = self.node.cu_count as u64;
            // average of the percentage, round to the nearest
            let den = s.count * cu_count;

            ((s.sum * 100 + den / 2) / den) as u32
        });
        let vram_usage = self.read_vram(pid).map(|v| v >> 10);

        KfdUsage {
            cu_occupancy,
            vram_usage,
        }
    }

    pub fn clear_samples(&mut self) {
        self.samples.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 8 CUs (Radeon 860M, gfx1152)
    const PROPERTIES: &str = "cpu_cores_count 0\n\
        simd_count 16\n\
        mem_banks_count 1\n\
        simd_per_cu 2\n\
        gfx_target_version 110502\n\
        drm_render_minor 128\n";
    const CPU_PROPERTIES: &str =
        "cpu_cores_count 16\nsimd_count 0\nsimd_per_cu 0\ndrm_render_minor 0\n";

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("amdgpu_top_kfd_{name}_{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();

            Self(root)
        }

        fn write(&self, path: &str, content: &str) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }

        fn topology(&self) {
            self.write("topology/nodes/0/gpu_id", "0\n");
            self.write("topology/nodes/0/properties", CPU_PROPERTIES);
            self.write("topology/nodes/1/gpu_id", "19622\n");
            self.write("topology/nodes/1/properties", PROPERTIES);
            self.write("topology/nodes/2/gpu_id", "4242\n");
            self.write(
                "topology/nodes/2/properties",
                "simd_count 256\nsimd_per_cu 4\ndrm_render_minor 1280\n",
            );
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parse_node() {
        let node = KfdNode::from_node_files("19622\n", PROPERTIES).unwrap();

        assert_eq!(
            node,
            KfdNode {
                gpu_id: 19622,
                cu_count: 8
            }
        );
    }

    #[test]
    fn parse_node_rejects_cpu_and_broken_files() {
        assert_eq!(KfdNode::from_node_files("0\n", CPU_PROPERTIES), None);
        // gpu_id 0 is never a GPU
        assert_eq!(KfdNode::from_node_files("0\n", PROPERTIES), None);
        assert_eq!(KfdNode::from_node_files("19622\n", CPU_PROPERTIES), None);
        assert_eq!(KfdNode::from_node_files("19622\n", "simd_count 16\n"), None);
        assert_eq!(
            KfdNode::from_node_files("19622\n", "simd_count 16\nsimd_per_cu 0\n"),
            None
        );
        assert_eq!(KfdNode::from_node_files("x\n", PROPERTIES), None);
    }

    #[test]
    fn property_key_is_exact() {
        // "simd_count" must not match "simd_count_x" or a prefix
        assert_eq!(
            parse_property("simd_count_x 3\nsimd_count 4\n", "simd_count"),
            Some(4)
        );
        assert_eq!(
            parse_property("drm_render_minor 1280\n", "drm_render_minor"),
            Some(1280)
        );
        assert_eq!(parse_property("simd_count\n", "simd_count"), None);
    }

    #[test]
    fn find_node_by_render_minor() {
        let fx = Fixture::new("find");
        fx.topology();

        assert_eq!(
            KfdNode::find(&fx.0, 128),
            Some(KfdNode {
                gpu_id: 19622,
                cu_count: 8
            }),
        );
        assert_eq!(
            KfdNode::find(&fx.0, 1280),
            Some(KfdNode {
                gpu_id: 4242,
                cu_count: 64
            }),
        );
        // the CPU node has drm_render_minor 0 but is not a GPU
        assert_eq!(KfdNode::find(&fx.0, 0), None);
        assert_eq!(KfdNode::find(&fx.0, 129), None);
    }

    #[test]
    fn find_node_without_kfd() {
        let fx = Fixture::new("nokfd");

        assert_eq!(KfdNode::find(&fx.0, 128), None);
    }

    #[test]
    fn occupancy_percent() {
        let node = KfdNode {
            gpu_id: 1,
            cu_count: 8,
        };

        assert_eq!(node.cu_occupancy_percent(0), 0);
        assert_eq!(node.cu_occupancy_percent(2), 25);
        assert_eq!(node.cu_occupancy_percent(3), 38); // 37.5
        assert_eq!(node.cu_occupancy_percent(8), 100);
        // more than the CU count (e.g. the CU count includes harvested CUs)
        assert_eq!(node.cu_occupancy_percent(9), 100);
        assert_eq!(node.cu_occupancy_percent(u32::MAX), 100);

        let node = KfdNode {
            gpu_id: 1,
            cu_count: 304,
        };
        assert_eq!(node.cu_occupancy_percent(152), 50);
    }

    #[test]
    fn average_samples_over_interval() {
        let fx = Fixture::new("avg");
        fx.topology();
        let node = KfdNode::find(&fx.0, 128).unwrap();
        let mut kfd = KfdStat::with_root(node, &fx.0);
        let file = "proc/100/stats_19622/cu_occupancy";

        for cu in ["0\n", "4\n", "8\n", "100\n"] {
            fx.write(file, cu);
            kfd.sample([100]);
        }

        // (0 + 4 + 8 + 8(capped)) / 4 CUs of 8 CUs
        assert_eq!(kfd.usage(100).cu_occupancy, Some(63)); // 62.5

        kfd.clear_samples();
        assert_eq!(kfd.usage(100).cu_occupancy, None);
    }

    #[test]
    fn missing_cu_occupancy_is_not_zero() {
        let fx = Fixture::new("missing");
        fx.topology();
        let node = KfdNode::find(&fx.0, 128).unwrap();
        let mut kfd = KfdStat::with_root(node, &fx.0);

        // no file at all (older kernel, unsupported GPU family)
        kfd.sample([100]);
        assert_eq!(kfd.usage(100).cu_occupancy, None);

        // the process has stats for another GPU only
        fx.write("proc/100/stats_1/cu_occupancy", "8\n");
        kfd.sample([100]);
        assert_eq!(kfd.usage(100).cu_occupancy, None);

        // garbage
        fx.write("proc/100/stats_19622/cu_occupancy", "\n");
        kfd.sample([100]);
        assert_eq!(kfd.usage(100).cu_occupancy, None);

        // an explicit 0 is 0%
        fx.write("proc/100/stats_19622/cu_occupancy", "0\n");
        kfd.sample([100]);
        assert_eq!(kfd.usage(100).cu_occupancy, Some(0));

        // a failed read does not dilute the average
        fx.write("proc/100/stats_19622/cu_occupancy", "8\n");
        kfd.sample([100]);
        fs::remove_file(fx.0.join("proc/100/stats_19622/cu_occupancy")).unwrap();
        kfd.sample([100]);
        assert_eq!(kfd.usage(100).cu_occupancy, Some(50));
    }

    #[test]
    fn read_vram() {
        let fx = Fixture::new("vram");
        fx.topology();
        let node = KfdNode::find(&fx.0, 128).unwrap();
        let kfd = KfdStat::with_root(node, &fx.0);

        assert_eq!(kfd.usage(100).vram_usage, None);

        fx.write("proc/100/vram_19622", "1073741824\n");
        fx.write("proc/100/vram_4242", "4096\n");

        assert_eq!(kfd.usage(100).vram_usage, Some(1 << 20));
    }

    #[test]
    fn sample_is_rate_limited() {
        let node = KfdNode {
            gpu_id: 1,
            cu_count: 8,
        };
        let mut kfd = KfdStat::with_root(node, "/nonexistent");

        assert!(kfd.is_sample_due());
        kfd.sample([]);
        assert!(!kfd.is_sample_due());
    }
}
