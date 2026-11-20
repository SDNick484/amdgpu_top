use std::fmt::{self, Write};
use crate::Opt;

use libamdgpu_top::stat::{FdInfoStat, FdInfoSortType};

// ref: drivers/gpu/drm/amd/amdgpu/amdgpu_fdinfo.c

const PROC_NAME_LEN: usize = 16;
const PID_MAX_LEN: usize = 7; // 2^22

const VRAM_LABEL: &str = "VRAM";
const GTT_LABEL: &str = "GTT";
const CPU_LABEL: &str = "CPU";
const GFX_LABEL: &str = "GFX";
const COMPUTE_LABEL: &str = "COMP";
const DMA_LABEL: &str = "DMA";
const DEC_LABEL: &str = "DEC";
const ENC_LABEL: &str = "ENC";
const VCN_UNIFIED_LABEL: &str = "VCNU";
const VPE_LABEL: &str = "VPE";
const KFD_LABEL: &str = "KFD";
const KFD_CU_LABEL: &str = "CU%";
// const UVD_ENC_LABEL: &str = "UVD (ENC)";
// const JPEG_LABEL: &str = "JPEG";

use crate::AppTextView;

impl AppTextView {
    pub const FDINFO_TITLE: &str = "fdinfo";

    pub fn print_fdinfo(
        &mut self,
        stat: &mut FdInfoStat,
        sort: FdInfoSortType,
        reverse: bool,
    ) -> Result<(), fmt::Error> {
        self.text.clear();

        write!(
            self.text.buf,
            " {proc_name:<PROC_NAME_LEN$}|{pid:^PID_MAX_LEN$}|{KFD_LABEL}|",
            proc_name = "Name",
            pid = "PID",
        )?;

        if stat.has_kfd_usage() {
            write!(self.text.buf, "{KFD_CU_LABEL:^4}|")?;
        }

        write!(
            self.text.buf,
            "{VRAM_LABEL:^6}|{GTT_LABEL:^6}|{CPU_LABEL:^4}|{GFX_LABEL:^4}|{COMPUTE_LABEL:^4}|{DMA_LABEL:^4}|",
        )?;

        if stat.has_vcn_unified {
            write!(self.text.buf, "{VCN_UNIFIED_LABEL:^4}|")?;
        } else {
            write!(self.text.buf, "{DEC_LABEL:^4}|{ENC_LABEL:^4}|")?;
        }

        if stat.has_vpe {
            write!(self.text.buf, "{VPE_LABEL:^4}|")?;
        }

        writeln!(self.text.buf)?;

        stat.sort_proc_usage(sort, reverse);

        self.print_fdinfo_usage(stat)?;

        Ok(())
    }

    pub fn print_fdinfo_usage(&mut self, stat: &FdInfoStat) -> Result<(), fmt::Error> {
        for pu in &stat.proc_usage {
            let utf16_count = pu.name.encode_utf16().filter(|&u| u >= 0x3000).count();
            let name_len = if pu.name.len() != utf16_count {
                PROC_NAME_LEN - utf16_count
            } else {
                PROC_NAME_LEN
            };
            write!(
                self.text.buf,
                " {name:name_len$}|{pid:>PID_MAX_LEN$}|{kfd:^3}|",
                name = pu.name,
                pid = pu.pid,
                kfd = if pu.is_kfd_process { "Y" } else { "" },
            )?;

            if stat.has_kfd_usage() {
                // blank: not a KFD process, N/A: the kernel does not provide cu_occupancy
                match (pu.is_kfd_process, pu.kfd.cu_occupancy) {
                    (_, Some(cu)) => write!(self.text.buf, "{cu:>3}%|")?,
                    (true, None) => write!(self.text.buf, "{:>4}|", "N/A")?,
                    (false, None) => write!(self.text.buf, "{:4}|", "")?,
                }
            }

            write!(
                self.text.buf,
                "{vram:>5}M|{gtt:>5}M|",
                vram = pu.usage.vram_usage >> 10,
                gtt = pu.usage.gtt_usage >> 10,
            )?;

            write!(self.text.buf, "{:>3}%|", pu.usage.cpu)?;

            for (usage, label_len) in [
                (pu.usage.gfx, GFX_LABEL.len()),
                (pu.usage.compute, COMPUTE_LABEL.len()-1),
                (pu.usage.dma, DMA_LABEL.len()),
            ] {
                write!(self.text.buf, "{usage:>label_len$}%|")?;
            }

            if stat.has_vcn_unified {
                write!(self.text.buf, "{:>3}%|", pu.usage.vcn_unified)?;
            } else {
                write!(self.text.buf, "{:>3}%|", pu.usage.total_dec)?;
                write!(self.text.buf, "{:>3}%|", pu.usage.total_enc)?;
            }

            if stat.has_vpe {
                write!(self.text.buf, "{:>3}%|", pu.usage.vpe)?;
            }

            writeln!(self.text.buf)?;
        }

        Ok(())
    }

    pub fn fdinfo_name(index: usize) -> String {
        format!("{} {index}", Self::FDINFO_TITLE)
    }

    pub fn cb_fdinfo(siv: &mut cursive::Cursive) {
        use crate::{set_min_height, set_visible_height, Opt};
        use cursive::views::TextView;

        let visible;
        let indexes = {
            let mut opt = siv.user_data::<Opt>().unwrap().lock().unwrap();
            opt.fdinfo ^= true;

            visible = opt.fdinfo;

            opt.indexes.clone()
        };

        for i in &indexes {
            let name = Self::fdinfo_name(*i);
            if visible {
                siv.call_on_name(&name, set_visible_height::<TextView>);
            } else {
                siv.call_on_name(&name, set_min_height::<TextView>);
            }
        }
    }

    pub fn cb_reverse_sort(siv: &mut cursive::Cursive) {
        {
            let mut opt = siv.user_data::<Opt>().unwrap().lock().unwrap();
            opt.reverse_sort ^= true;
        }
    }

    pub fn cb_sort_by_pid(siv: &mut cursive::Cursive) {
        {
            let mut opt = siv.user_data::<Opt>().unwrap().lock().unwrap();
            opt.fdinfo_sort = FdInfoSortType::PID;
        }
    }

    pub fn cb_sort_by_vram(siv: &mut cursive::Cursive) {
        {
            let mut opt = siv.user_data::<Opt>().unwrap().lock().unwrap();
            opt.fdinfo_sort = FdInfoSortType::VRAM;
        }
    }

    pub fn cb_sort_by_cpu(siv: &mut cursive::Cursive) {
        {
            let mut opt = siv.user_data::<Opt>().unwrap().lock().unwrap();
            opt.fdinfo_sort = FdInfoSortType::CPU;
        }
    }

    pub fn cb_sort_by_gfx(siv: &mut cursive::Cursive) {
        {
            let mut opt = siv.user_data::<Opt>().unwrap().lock().unwrap();
            opt.fdinfo_sort = FdInfoSortType::GFX;
        }
    }

    pub fn cb_sort_by_media(siv: &mut cursive::Cursive) {
        {
            let mut opt = siv.user_data::<Opt>().unwrap().lock().unwrap();
            opt.fdinfo_sort = FdInfoSortType::VCNU;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libamdgpu_top::stat::{KfdNode, KfdStat, KfdUsage, ProcUsage};

    fn print(stat: &mut FdInfoStat) -> Vec<String> {
        let mut view = AppTextView::default();
        view.print_fdinfo(stat, FdInfoSortType::PID, true).unwrap();

        view.text.buf.lines().map(str::to_string).collect()
    }

    #[test]
    fn kfd_cu_column() {
        let proc_usage = vec![
            ProcUsage {
                pid: 1,
                name: "hip".to_string(),
                is_kfd_process: true,
                kfd: KfdUsage {
                    cu_occupancy: Some(99),
                    vram_usage: None,
                },
                ..Default::default()
            },
            ProcUsage {
                pid: 2,
                name: "old_kernel".to_string(),
                is_kfd_process: true,
                ..Default::default()
            },
            ProcUsage {
                pid: 3,
                name: "game".to_string(),
                ..Default::default()
            },
        ];
        let mut stat = FdInfoStat {
            kfd: Some(KfdStat::new(KfdNode {
                gpu_id: 1,
                cu_count: 8,
            })),
            proc_usage,
            ..Default::default()
        };

        let lines = print(&mut stat);
        let cols: Vec<Vec<&str>> = lines.iter().map(|l| l.split('|').collect()).collect();

        assert_eq!(cols[0][3], "CU% ");
        assert_eq!(cols[1][3], " 99%");
        assert_eq!(cols[2][3], " N/A"); // not 0%
        assert_eq!(cols[3][3], "    ");
        // the same columns as without KFD
        assert_eq!(cols[0].len(), cols[1].len());

        // no KFD node: no column
        stat.kfd = None;
        let lines = print(&mut stat);

        assert!(!lines[0].contains("CU%"));
        assert_eq!(lines[1].split('|').count(), cols[1].len() - 1);
    }
}
