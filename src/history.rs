use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::config::Config;

/// 一次访问：打开的网址、标题、当天日期、时刻（当天的秒数）、时长
pub type Visit = (String, String, String, i64, i64);

#[derive(Clone, serde::Serialize)]
pub struct Link {
    pub url: String,
    pub host: String,
    pub title: String,
    /// 当天在该网址上停留的秒数（同一网址多次访问相加）
    pub secs: i64,
    /// 24 小时制 H:MM:SS
    pub time: String,
    /// 第一次打开的先后，同一天内从小到大
    pub order: i64,
}

/// 秒数 → 时长：`30s`、`15min 20s`、`1h 18min 3s`（秒取舍入；为 0 时记 `<1s`）
pub fn dur(secs: i64) -> String {
    let s = secs.max(0);
    let (h, m, s) = (s / 3600, s % 3600 / 60, s % 60);
    if h > 0 {
        format!("{h}h {m}min {s}s")
    } else if m > 0 {
        format!("{m}min {s}s")
    } else if s > 0 {
        format!("{s}s")
    } else {
        "<1s".into()
    }
}

/// 日期（YYYY-MM-DD，本地时间）→ 当天打开过的页面
pub type Days = BTreeMap<String, Vec<Link>>;

fn profiles(cfg: &Config) -> Vec<PathBuf> {
    let root = cfg.chrome_root();
    let names: Vec<String> = if cfg.profiles.is_empty() {
        let mut names: Vec<String> = fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n == "Default" || n.strip_prefix("Profile ").is_some_and(|d| d.parse::<u32>().is_ok()))
            .collect();
        names.sort();
        names
    } else {
        cfg.profiles.clone()
    };
    names.iter().map(|n| root.join(n).join("History")).filter(|p| p.exists()).collect()
}

/// Chrome 运行时数据库是锁着的，所以先复制到临时目录再打开。
/// `sql` 里的 ?1/?2 是 since/until。
fn query<T>(
    path: &Path,
    sql: &str,
    since: &str,
    until: &str,
    row: impl Fn(&rusqlite::Row) -> rusqlite::Result<T>,
) -> Result<Vec<T>, Box<dyn Error>> {
    let copy = std::env::temp_dir().join(format!("bhr-history-{}", std::process::id()));
    fs::copy(path, &copy)?;
    let rows = (|| -> rusqlite::Result<_> {
        let db = Connection::open_with_flags(&copy, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut stmt = db.prepare(sql)?;
        stmt.query_map([since, until], row)?.collect::<rusqlite::Result<Vec<_>>>()
    })();
    let _ = fs::remove_file(&copy);
    Ok(rows?)
}

// 「本地时间」一律按北京时间（UTC+8）算，不看这台机器的时区设置：
// date(...) 给的是日期，t 是当天的秒数（用 'utc' 取，再加 8 小时）。
// visit_time / visit_duration 都是自 1601-01-01 起的微秒数。
// transition 低 8 位为 3/4 的是 iframe（不是本人打开的页面）。
const VISITS: &str = "SELECT u.url, u.title,
                    date(v.visit_time / 1000000 - 11644473600 + 28800, 'unixepoch') AS d,
                    (v.visit_time / 1000000 - 11644473600 + 28800) % 86400 AS t,
                    v.visit_duration / 1000000 AS sec
               FROM visits v JOIN urls u ON u.id = v.url
              WHERE (v.transition & 255) NOT IN (3, 4)
                AND u.title IS NOT NULL AND u.title <> ''
                AND d >= ?1 AND d <= ?2
              ORDER BY v.visit_time";

/// 秒数 → `H:MM:SS`（北京时间，与 SQL 里的 +28800 对应）
pub fn hms(t: i64) -> String {
    format!("{}:{:02}:{:02}", t.max(0) / 3600, t.max(0) % 3600 / 60, t.max(0) % 60)
}

// 输入到搜索框里的词，以及从该搜索结果页打开的页面（顺着 from_visit 找）。
// ponytail: 只往上追一层。若中间隔着跳转而漏掉，再考虑递归追 from_visit
const SEARCHES: &str = "SELECT k.term, k.normalized_term,
                    date(v.visit_time / 1000000 - 11644473600 + 28800, 'unixepoch') AS d,
                    cu.url, cu.title
               FROM keyword_search_terms k JOIN visits v ON v.url = k.url_id
               LEFT JOIN visits c ON c.from_visit = v.id
               LEFT JOIN urls cu ON cu.id = c.url
              WHERE d >= ?1 AND d <= ?2
              ORDER BY v.visit_time, c.visit_time";

#[derive(Clone, serde::Serialize)]
pub struct Search {
    pub term: String,
    /// 从搜索结果打开的页面。即使搜索引擎在 deny 里也保留搜索词本身，只有页面走 normalize()
    pub links: Vec<Link>,
}

/// 一天的页面怎么排
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    /// 按时间顺序（当天第一次打开的先后）
    Time,
    /// 按时长从长到短
    Duration,
    /// 按主机分组
    Host,
}

/// 按 `sort` 重排一天里的页面
pub fn sort_links(links: &mut [Link], sort: Sort) {
    links.sort_by(|a, b| match sort {
        Sort::Time => a.order.cmp(&b.order),
        Sort::Duration => b.secs.cmp(&a.secs).then_with(|| a.order.cmp(&b.order)),
        Sort::Host => a.host.cmp(&b.host).then_with(|| b.secs.cmp(&a.secs)),
    });
}

/// 读出区间内的访问（已规整、已过滤），按时间先后。时长按 `max_visit_min` 单次封顶。
pub fn visits(cfg: &Config, since: Option<&str>, until: Option<&str>, max_visit_min: i64) -> Result<Vec<Visit>, Box<dyn Error>> {
    let paths = profiles(cfg);
    if paths.is_empty() {
        return Err(format!("找不到 History 文件: {}", cfg.chrome_root().display()).into());
    }
    let mut out = Vec::new();
    for path in paths {
        for (raw, title, date, t, sec) in query(&path, VISITS, since.unwrap_or(""), until.unwrap_or("9999"), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })? {
            let Some((url, _)) = cfg.normalize(&raw) else { continue };
            let title: String = title.trim().chars().take(200).collect();
            // 单次时长封顶：标签页一直挂着不关时，Chrome 会记下以小时甚至天计的时长
            let sec = if max_visit_min > 0 { sec.min(max_visit_min * 60) } else { sec };
            out.push((url, title, date, t, sec));
        }
    }
    out.sort_by(|a, b| a.2.cmp(&b.2).then(a.3.cmp(&b.3)));
    Ok(out)
}

/// 访问 → 每天一行的页面。同一天里「主机 + 标题」相同的页面只留一行（键不是 URL，因为搜索框里逐字输入会留下各自的 URL），
/// 保留最短的 URL。时长按网址汇总：网址相同就累加，网址不同则各自计时；行上的时长取保留的那个网址。
/// 时刻取第一次打开的时间。
pub fn load(cfg: &Config, since: Option<&str>, until: Option<&str>, max_visit_min: i64) -> Result<Days, Box<dyn Error>> {
    let mut days: BTreeMap<String, HashMap<(String, String), Link>> = BTreeMap::new();
    // 日期 →（主机+标题）→（网址 → 秒数）
    let mut urls: HashMap<(String, (String, String)), HashMap<String, i64>> = HashMap::new();
    for (url, title, date, t, sec) in visits(cfg, since, until, max_visit_min)? {
        let host = cfg.host_of(&url);
        let key = (host.clone(), title.clone());
        let seen = urls.entry((date.clone(), key.clone())).or_default();
        *seen.entry(url.clone()).or_default() += sec;
        let best = seen.iter().min_by_key(|(u, _)| u.len()).map_or(url.clone(), |(u, _)| u.clone());
        let secs = seen[&best];
        let day = days.entry(date).or_default();
        match day.get_mut(&key) {
            Some(line) => line.secs = secs,
            None => {
                let order = day.len() as i64;
                day.insert(key, Link { url: best, host, title, secs, time: hms(t), order });
            }
        }
    }
    Ok(days
        .into_iter()
        .map(|(date, day)| {
            let mut links: Vec<Link> = day.into_values().collect();
            links.sort_by(|a, b| a.host.cmp(&b.host).then_with(|| a.url.cmp(&b.url)));
            (date, links)
        })
        .collect())
}


/// 日期 → 当天搜索过的词（按第一次输入的顺序，同一个词只留一次）
pub type Searches = BTreeMap<String, Vec<Search>>;

pub fn searches(cfg: &Config, since: Option<&str>, until: Option<&str>) -> Result<Searches, Box<dyn Error>> {
    let mut days: BTreeMap<String, Vec<(String, Search)>> = BTreeMap::new();
    for path in profiles(cfg) {
        let rows = query(&path, SEARCHES, since.unwrap_or(""), until.unwrap_or("9999"), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?;
        for (term, norm, date, url, title) in rows {
            let day = days.entry(date).or_default();
            let i = match day.iter().position(|(n, _)| *n == norm) {
                Some(i) => i,
                None => {
                    day.push((norm, Search { term, links: Vec::new() }));
                    day.len() - 1
                }
            };
            let (Some(raw), Some(title)) = (url, title) else { continue };
            let Some((url, host)) = cfg.normalize(&raw) else { continue };
            let title: String = title.trim().chars().take(200).collect();
            let links = &mut day[i].1.links;
            if !title.is_empty() && !links.iter().any(|l| l.url == url) {
                links.push(Link { url, host, title, secs: 0, time: String::new(), order: 0 });
            }
        }
    }
    Ok(days.into_iter().map(|(d, s)| (d, s.into_iter().map(|(_, s)| s).collect())).collect())
}

fn label(title: &str) -> String {
    title.replace(['[', ']'], "").split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn render_searches(date: &str, searches: &[Search]) -> String {
    let mut out = format!("# {date} 的搜索词\n\n");
    for s in searches {
        out.push_str(&format!("- {}\n", s.term));
        for l in &s.links {
            out.push_str(&format!("  - [{}]({})\n", label(&l.title), l.url));
        }
    }
    out
}

/// `scores`（相关度）有就按它排；否则按 `sort` 排。`time` 时行首加打开时刻，`durs` 时每行加时长。
pub fn render(date: &str, links: &[Link], scores: Option<&[f64]>, sort: Sort, time: bool, durs: bool) -> String {
    let mut out = format!("# {date} 打开的网页\n\n");
    let mut host = "";
    for (i, link) in links.iter().enumerate() {
        let label = label(&link.title);
        let mut row = String::new();
        if time {
            row.push_str(&format!("{} ", link.time));
        }
        if durs {
            row.push_str(&format!("{} ", dur(link.secs)));
        }
        if let Some(s) = scores {
            row.push_str(&format!("{:.2} ", s[i]));
        }
        row.push_str(&format!("[{label}]({}) — {}", link.url, link.host));
        match sort {
            // 按主机分组时，时长和时刻放在分组下面
            Sort::Host => {
                if link.host != host {
                    host = &link.host;
                    out.push_str(&format!("\n## {host}\n\n"));
                }
                out.push_str(&format!("- {row}\n"));
            }
            Sort::Time | Sort::Duration => out.push_str(&format!("- {row}\n")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_is_exact() {
        assert_eq!(dur(30), "30s");
        assert_eq!(dur(920), "15min 20s");
        assert_eq!(dur(4683), "1h 18min 3s");
        assert_eq!(dur(3600), "1h 0min 0s");
        assert_eq!(dur(60), "1min 0s");
        assert_eq!(dur(0), "<1s");
    }

    #[test]
    fn sort_modes() {
        let mut links = vec![link("https://a.com/x", "A", 30, "9:00:00"), link("https://b.com/y", "B", 900, "8:00:00")];
        for (i, l) in links.iter_mut().enumerate() {
            l.order = i as i64;
        }
        let order = |v: &[Link]| v.iter().map(|l| l.title.clone()).collect::<Vec<_>>();
        let mut v = links.clone();
        sort_links(&mut v, Sort::Time);
        assert_eq!(order(&v), ["A", "B"]);
        sort_links(&mut v, Sort::Duration);
        assert_eq!(order(&v), ["B", "A"]);
        sort_links(&mut v, Sort::Host);
        assert_eq!(order(&v), ["A", "B"]);
    }

    #[test]
    fn time_of_day() {
        assert_eq!(hms(0), "0:00:00");
        assert_eq!(hms(9 * 3600 + 5 * 60 + 7), "9:05:07");
        assert_eq!(hms(23 * 3600 + 59 * 60 + 59), "23:59:59");
    }

    fn link(url: &str, title: &str, secs: i64, time: &str) -> Link {
        let host = url.split("//").nth(1).unwrap().split('/').next().unwrap().to_string();
        Link { url: url.into(), host, title: title.into(), secs, time: time.into(), order: 0 }
    }

    #[test]
    fn renders_by_time_with_time_and_duration() {
        let links = vec![link("https://a.com/x", "A", 920, "9:05:07"), link("https://b.com/y", "B", 4683, "10:00:00")];
        let md = render("2026-11-26", &links, None, Sort::Time, true, true);
        assert_eq!(
            md,
            "# 2026-11-26 打开的网页\n\n- 9:05:07 15min 20s [A](https://a.com/x) — a.com\n- 10:00:00 1h 18min 3s [B](https://b.com/y) — b.com\n"
        );
        let md = render("2026-11-26", &links, None, Sort::Duration, false, true);
        assert_eq!(md, "# 2026-11-26 打开的网页\n\n- 15min 20s [A](https://a.com/x) — a.com\n- 1h 18min 3s [B](https://b.com/y) — b.com\n");
    }

    #[test]
    fn renders_grouped_by_host() {
        let links = vec![link("https://a.com/x", "A", 30, "9:00:00")];
        let md = render("2026-11-26", &links, None, Sort::Host, false, false);
        assert_eq!(md, "# 2026-11-26 打开的网页\n\n\n## a.com\n\n- [A](https://a.com/x) — a.com\n");
    }
}
