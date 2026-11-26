mod ask;
mod config;
mod history;
mod tui;

use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use history::{sort_links, Link, Sort};

/// 从 Chrome 历史记录里，按天列出读过的页面
#[derive(Parser)]
struct Cli {
    /// 配置文件（默认: ~/.config/bhr/config.toml）
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// 从这个日期开始（YYYY-MM-DD）
    #[arg(long, global = true, value_parser = date)]
    since: Option<String>,
    /// 到这个日期为止（YYYY-MM-DD）
    #[arg(long, global = true, value_parser = date)]
    until: Option<String>,
    /// 以 JSON 输出（report / hosts）
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// 在 TUI 中查看（默认）
    Tui,
    /// 按天列出打开的页面（Markdown / --json）
    #[command(alias = "list")]
    Report {
        /// 只看一天（YYYY-MM-DD）
        #[arg(long, value_parser = date)]
        day: Option<String>,
        /// 用 jev 按语义相关度打分，按天按匹配度排序（一页一次请求）
        #[arg(long)]
        ask: Option<String>,
        /// 显示每行的时长（30s / 15min 20s / 1h 18min 3s）
        #[arg(long)]
        dur: bool,
        /// 显示打开的时刻（H:MM:SS）
        #[arg(long)]
        time: bool,
        /// 排序方式：time 按时间顺序，duration 按时长从长到短，host 按主机分组
        #[arg(long, value_enum, default_value_t = SortArg::Time)]
        sort: SortArg,
    },
    /// 按天列出搜索过的词（Markdown / --json）
    Searches {
        /// 只看一天（YYYY-MM-DD）
        #[arg(long, value_parser = date)]
        day: Option<String>,
    },
    /// 把收录的主机按条数排序（用来找要加进 deny 的）
    Hosts {
        #[arg(long, default_value_t = 50)]
        top: usize,
        /// 排序方式：duration 按时长从长到短，count 按页面条数
        #[arg(long, value_enum, default_value_t = HostArg::Duration)]
        sort: HostArg,
    },
    /// 显示配置文件的位置
    Config,
}

#[derive(Clone, Copy, PartialEq, clap::ValueEnum)]
enum SortArg {
    /// 按时间顺序
    Time,
    /// 按时长从长到短
    Duration,
    /// 按主机分组
    Host,
}

#[derive(Clone, Copy, PartialEq, clap::ValueEnum)]
enum HostArg {
    /// 按时长从长到短
    Duration,
    /// 按页面条数
    Count,
}

impl From<SortArg> for Sort {
    fn from(s: SortArg) -> Self {
        match s {
            SortArg::Time => Self::Time,
            SortArg::Duration => Self::Duration,
            SortArg::Host => Self::Host,
        }
    }
}

fn date(s: &str) -> Result<String, String> {
    let ok = s.len() == 10
        && s.char_indices().all(|(i, c)| if i == 4 || i == 7 { c == '-' } else { c.is_ascii_digit() });
    if ok { Ok(s.to_string()) } else { Err("请用 YYYY-MM-DD 格式".into()) }
}

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let path = cli.config.unwrap_or_else(config::default_path);
    // 时长按网址汇总，不能再按主机过滤，所以从这里就不再借出 cfg；只把时长上限传给 history
    let cfg = config::load(&path)?;
    let (max_visit, min_score) = (cfg.max_visit_min, cfg.min_score);
    let (mut since, mut until) = (cli.since, cli.until);

    match cli.cmd.unwrap_or(Cmd::Tui) {
        Cmd::Config => println!("{}", path.display()),
        Cmd::Tui => {
            let days = history::load(&cfg, since.as_deref(), until.as_deref(), max_visit)?;
            let searches = history::searches(&cfg, since.as_deref(), until.as_deref())?;
            tui::run(cfg, &path, days, searches)?;
        }
        Cmd::Report { day, ask, dur, time, sort } => {
            if day.is_some() {
                (since, until) = (day.clone(), day);
            }
            let mut days: Vec<(String, Vec<Link>, Option<Vec<f64>>)> = Vec::new();
            for (date, links) in history::load(&cfg, since.as_deref(), until.as_deref(), max_visit)? {
                let Some(query) = &ask else {
                    days.push((date, links, None));
                    continue;
                };
                let mut scored: Vec<(Link, f64)> = links.iter().cloned().zip(ask::relevance(&links, query)?).collect();
                scored.retain(|(_, s)| *s >= min_score);
                if scored.is_empty() {
                    continue;
                }
                scored.sort_by(|a, b| b.1.total_cmp(&a.1));
                let (links, scores) = scored.into_iter().unzip();
                days.push((date, links, Some(scores)));
            }
            if cli.json {
                let mut out = Vec::new();
                for (date, links, scores) in &days {
                    let mut links = serde_json::to_value(links)?;
                    if let (Some(scores), Some(links)) = (scores, links.as_array_mut()) {
                        for (link, s) in links.iter_mut().zip(scores) {
                            link["score"] = serde_json::json!(s);
                        }
                    }
                    out.push(serde_json::json!({ "date": date, "links": links }));
                }
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                // 有 --ask 时由相关度决定顺序（总是显示时长），否则按 --sort 排
                let sort = if ask.is_some() { Sort::Duration } else { sort.into() };
                if ask.is_none() {
                    for (_, links, _) in &mut days {
                        sort_links(links, sort);
                    }
                }
                let pages: Vec<String> = days
                    .iter()
                    .map(|(d, links, scores)| history::render(d, links, scores.as_deref(), sort, time, dur || ask.is_some()))
                    .collect();
                print!("{}", pages.join("\n"));
            }
        }
        Cmd::Searches { day } => {
            if day.is_some() {
                (since, until) = (day.clone(), day);
            }
            let days = history::searches(&cfg, since.as_deref(), until.as_deref())?;
            if cli.json {
                let out: Vec<_> =
                    days.iter().map(|(d, s)| serde_json::json!({ "date": d, "searches": s })).collect();
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                let pages: Vec<String> = days.iter().map(|(d, s)| history::render_searches(d, s)).collect();
                print!("{}", pages.join("\n"));
            }
        }
        Cmd::Hosts { top, sort } => {
            let days = history::load(&cfg, since.as_deref(), until.as_deref(), max_visit)?;
            let mut count: HashMap<&str, (usize, i64)> = HashMap::new();
            for link in days.values().flatten() {
                let e = count.entry(&link.host).or_default();
                e.0 += 1;
                e.1 += link.secs;
            }
            let mut count: Vec<_> = count.into_iter().collect();
            match sort {
                HostArg::Duration => count.sort_by(|a, b| b.1 .1.cmp(&a.1 .1).then(a.0.cmp(b.0))),
                HostArg::Count => count.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then(a.0.cmp(b.0))),
            }
            count.truncate(top);
            // 最长的一条时长：用来对齐那一列
            let width = count.iter().map(|(_, (_, secs))| history::dur(*secs).chars().count()).max().unwrap_or(6);
            if cli.json {
                let out: Vec<_> = count
                    .iter()
                    .map(|(host, (c, secs))| {
                        serde_json::json!({ "host": host, "count": c, "duration": history::dur(*secs), "secs": secs, "minutes": (secs.max(&0) + 59) / 60 })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                for (host, (c, secs)) in count {
                    println!("{c:>6}  {:>width$}  {host}", history::dur(secs));
                }
            }
        }
    }
    Ok(())
}
