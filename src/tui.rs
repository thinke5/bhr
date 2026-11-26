use std::collections::HashMap;
use std::error::Error;
use std::path::Path;
use std::process::{Command, Stdio};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListState, Paragraph};

use crate::ask;
use crate::config::{self, Config};
use crate::history::{self, Days, Link, Searches, Sort};

#[derive(PartialEq)]
enum Input {
    None,
    /// 用 `/` 按文字筛选
    Filter,
    /// 用 `?` 问 jev 语义
    Ask,
}

/// 左侧的行。选月份行时右侧显示整月，选日期行时只显示那一天
enum Row {
    Month(String),
    Day(usize),
}

struct App {
    /// 只经过排除过滤的全部数据。`days` 是在这之上按搜索词筛选的结果
    all: Vec<(String, Vec<Link>)>,
    days: Vec<(String, Vec<Link>)>,
    /// 左侧排列的行。只有光标所在的月份才列出日期
    rows: Vec<Row>,
    /// 右侧排列的（日期，页面）。由选中的行生成，jev 会重排它
    view: Vec<(String, Link)>,
    query: String,
    ask: String,
    input: Input,
    /// URL → jev 给出的「符合搜索」概率
    scores: HashMap<String, f64>,
    row: ListState,
    link: ListState,
    on_links: bool,
    status: String,
    /// 日期 → 搜索过的词。在右下显示选中月/日的内容（不应用筛选）
    searches: Searches,
    /// 右侧排序方式（时间顺序 / 时长 / 按域名）
    sort: Sort,
    /// 是否在行首显示打开的时刻
    show_time: bool,
    /// 右下角显示搜索词，还是按域名聚合的合计
    panel: Panel,
}

/// 右下角的框显示什么
#[derive(Clone, Copy, PartialEq)]
enum Panel {
    Searches,
    Hosts,
}

impl Sort {
    fn label(self) -> &'static str {
        match self {
            Sort::Time => "时间顺序",
            Sort::Duration => "按时长",
            Sort::Host => "按域名",
        }
    }

    fn next(self) -> Self {
        match self {
            Sort::Time => Sort::Duration,
            Sort::Duration => Sort::Host,
            Sort::Host => Sort::Time,
        }
    }
}

/// 按域名合计（条数、秒数），按时长从长到短
fn hosts(links: &[(String, Link)]) -> Vec<(String, usize, i64)> {
    let mut map: HashMap<&str, (usize, i64)> = HashMap::new();
    for (_, l) in links {
        let e = map.entry(l.host.as_str()).or_default();
        e.0 += 1;
        e.1 += l.secs;
    }
    let mut out: Vec<(String, usize, i64)> = map.into_iter().map(|(h, (c, s))| (h.to_string(), c, s)).collect();
    out.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    out
}

/// 按空白分开的每个词是否都出现在标题、主机或 URL 中（不区分大小写）
fn hit(link: &Link, terms: &[String]) -> bool {
    let hay = format!("{}\n{}\n{}", link.title, link.host, link.url).to_lowercase();
    terms.iter().all(|t| hay.contains(t.as_str()))
}

impl App {
    fn refilter(&mut self) {
        let terms: Vec<String> = self.query.to_lowercase().split_whitespace().map(String::from).collect();
        self.days = self
            .all
            .iter()
            .map(|(d, links)| (d.clone(), links.iter().filter(|l| hit(l, &terms)).cloned().collect::<Vec<_>>()))
            .filter(|(_, links)| !links.is_empty())
            .collect();
        // 相关度顺序会恢复原样，所以丢掉 jev 的分数，回到按 sort 排的次序
        self.scores.clear();
        self.sort_view();
        let i = self.link.selected().unwrap_or(0);
        self.select(&self.key());
        if !self.view.is_empty() {
            self.link.select(Some(i.min(self.view.len() - 1)));
        }
    }

    /// 没有被 jev 重排时，按 sort 重排右侧
    fn sort_view(&mut self) {
        if self.scores.is_empty() {
            self.view.sort_by(|(_, a), (_, b)| match self.sort {
                Sort::Time => a.order.cmp(&b.order),
                Sort::Duration => b.secs.cmp(&a.secs).then_with(|| a.order.cmp(&b.order)),
                Sort::Host => a.host.cmp(&b.host).then_with(|| b.secs.cmp(&a.secs)),
            });
        }
    }

    fn label(&self, row: &Row) -> String {
        match row {
            Row::Month(m) => m.clone(),
            Row::Day(i) => self.days[*i].0.clone(),
        }
    }

    /// 选中行代表的月份（YYYY-MM）或日期（YYYY-MM-DD）
    fn key(&self) -> String {
        self.row.selected().and_then(|i| self.rows.get(i)).map_or(String::new(), |r| self.label(r))
    }

    fn on_month(&self) -> bool {
        matches!(self.row.selected().and_then(|i| self.rows.get(i)), Some(Row::Month(_)))
    }

    /// 选中 `key`（月或日）对应的行，只展开那个月的日期并重排。已消失的话跳到最新的一天
    fn select(&mut self, key: &str) {
        let key = match self.days.first() {
            Some(_) if !key.is_empty() && self.days.iter().any(|(d, _)| d.starts_with(key)) => key.to_string(),
            Some((d, _)) => d.clone(),
            None => String::new(),
        };
        let open = &key[..key.len().min(7)];
        self.rows.clear();
        for (i, (d, _)) in self.days.iter().enumerate() {
            if i == 0 || d[..7] != self.days[i - 1].0[..7] {
                self.rows.push(Row::Month(d[..7].to_string()));
            }
            if &d[..7] == open {
                self.rows.push(Row::Day(i));
            }
        }
        let at = self.rows.iter().position(|r| self.label(r) == key).unwrap_or(0);
        self.row.select((!self.rows.is_empty()).then_some(at));
        self.view = self
            .days
            .iter()
            .filter(|(d, _)| d.starts_with(&key))
            .flat_map(|(d, links)| links.iter().map(move |l| (d.clone(), l.clone())))
            .collect();
        self.sort_view();
        self.link.select((!self.view.is_empty()).then_some(0));
    }

    /// 让 jev 给右侧页面打分，按匹配度重新排序。
    fn rank(&mut self, min_score: f64) {
        let links: Vec<Link> = self.view.iter().map(|(_, l)| l.clone()).collect();
        match ask::relevance(&links, &self.ask) {
            Ok(scores) => {
                for (link, s) in links.iter().zip(&scores) {
                    self.scores.insert(link.url.clone(), *s);
                }
                let score = |l: &Link| self.scores.get(&l.url).copied().unwrap_or(0.0);
                let before = self.view.len();
                self.view.retain(|(_, l)| score(l) >= min_score);
                self.view.sort_by(|(_, a), (_, b)| score(b).total_cmp(&score(a)));
                let hits = self.view.len();
                self.link.select((hits > 0).then_some(0));
                self.on_links = hits > 0;
                self.status = format!("jev：「{}」匹配度排序  {hits}/{before} 条（≥{min_score}，Esc 恢复）", self.ask);
            }
            Err(e) => self.status = format!("问不了 jev: {e}"),
        }
    }

    fn current(&self) -> Option<&Link> {
        self.link.selected().and_then(|i| self.view.get(i)).map(|(_, l)| l)
    }

    fn step(&mut self, down: bool) {
        let (cur, len) = if self.on_links {
            (self.link.selected(), self.view.len())
        } else {
            (self.row.selected(), self.rows.len())
        };
        if len == 0 {
            return;
        }
        let i = cur.unwrap_or(0);
        let i = if down { (i + 1).min(len - 1) } else { i.saturating_sub(1) };
        if self.on_links {
            self.link.select(Some(i));
        } else {
            self.select(&self.label(&self.rows[i]));
        }
    }
}

fn open(url: &str) -> std::io::Result<()> {
    let cmd = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    Command::new(cmd).arg(url).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(|_| ())
}

fn draw(f: &mut Frame, app: &mut App) {
    let [main, foot] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
    let [left, right] = Layout::horizontal([Constraint::Length(20), Constraint::Min(1)]).areas(main);
    let key = app.key();
    let month = app.on_month();
    // 从新到旧。选月份时在行首加上日期
    let terms: Vec<Line> = app
        .searches
        .iter()
        .rev()
        .filter(|(d, _)| !key.is_empty() && d.starts_with(&key))
        .flat_map(|(d, searches)| {
            searches.iter().map(move |s| {
                let mut spans = Vec::new();
                if month {
                    spans.push(Span::styled(format!("{} ", &d[5..]), Style::default().fg(Color::Cyan)));
                }
                spans.push(Span::raw(s.term.clone()));
                // 只显示打开页面的主机（压缩到一行）
                if !s.links.is_empty() {
                    let hosts: Vec<&str> = s.links.iter().map(|l| l.host.as_str()).collect();
                    spans.push(Span::styled(format!("  → {}", hosts.join(", ")), Style::default().fg(Color::Yellow)));
                }
                Line::from(spans)
            })
        })
        .collect();
    // 右下角：默认显示搜索词，按 S 换成按域名聚合的合计
    let below_title = if app.panel == Panel::Searches {
        "搜索词"
    } else {
        "域名合计"
    };
    let height_of_searches = terms.len();
    let below = if app.panel == Panel::Searches {
        terms
    } else {
        let mut list: Vec<Line> = Vec::new();
        let hosts = hosts(&app.view);
        let width = hosts.iter().map(|(_, _, s)| history::dur(*s).len()).max().unwrap_or(4);
        for (host, count, secs) in hosts {
            let mut spans = Vec::new();
            if month {
                spans.push(Span::styled(format!("{} ", &key[5..]), Style::default().fg(Color::Cyan)));
            }
            spans.push(Span::styled(format!("{:>width$} ", history::dur(secs)), Style::default().fg(Color::Cyan)));
            spans.push(Span::styled(format!("{count:>4} "), Style::default().fg(Color::Green)));
            spans.push(Span::raw(host));
            list.push(Line::from(spans));
        }
        list
    };
    let height = match app.panel {
        Panel::Searches => height_of_searches,
        Panel::Hosts => app.view.iter().map(|(_, l)| l.host.as_str()).collect::<std::collections::HashSet<_>>().len(),
    };
    // ponytail: 下框不能滚动，超过的行看不见。不够用时再让框架自己持有光标
    let [right, below_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length((height as u16).min(8) + 2)]).areas(right);
    f.render_widget(
        Paragraph::new(below)
            .block(Block::bordered().title(below_title).border_style(Style::default().fg(Color::DarkGray))),
        below_area,
    );
    let focus = |on: bool| Style::default().fg(if on { Color::Cyan } else { Color::DarkGray });
    let hl = Style::default().add_modifier(Modifier::REVERSED);

    let rows = List::new(app.rows.iter().map(|r| match r {
        Row::Month(m) => {
            let n: usize = app.days.iter().filter(|(d, _)| d.starts_with(m.as_str())).map(|(_, l)| l.len()).sum();
            Line::styled(format!("{m} {n:>7}"), Style::default().add_modifier(Modifier::BOLD))
        }
        Row::Day(i) => {
            let (d, l) = &app.days[*i];
            Line::raw(format!("  {} {:>7}", &d[5..], l.len()))
        }
    }))
    .block(Block::bordered().title("月·日").border_style(focus(!app.on_links)))
    .highlight_style(hl);
    f.render_stateful_widget(rows, left, &mut app.row);

    // 时长在最左，按最长的 "1h 18min 3s" 对齐列宽；可选的时刻在它左边；选月份时行首再加日期
    let secs = app.view.iter().map(|(_, l)| history::dur(l.secs).len()).max().unwrap_or(0).max(4);
    let links = List::new(app.view.iter().map(|(d, l)| {
        let mut spans = Vec::new();
        if month {
            spans.push(Span::styled(format!("{} ", &d[5..]), Style::default().fg(Color::Cyan)));
        }
        if app.show_time {
            spans.push(Span::styled(format!("{} ", l.time), Style::default().fg(Color::DarkGray)));
        }
        spans.push(Span::styled(format!("{:>secs$} ", history::dur(l.secs)), Style::default().fg(Color::Cyan)));
        if let Some(s) = app.scores.get(&l.url) {
            spans.push(Span::styled(format!("{s:.2} "), Style::default().fg(Color::Green)));
        }
        spans.push(Span::styled(format!("{:<24} ", l.host), Style::default().fg(Color::Yellow)));
        spans.push(Span::raw(l.title.clone()));
        Line::from(spans)
    }))
    .block(Block::bordered().title(format!("打开的网页·{}", app.sort.label())).border_style(focus(app.on_links)))
    .highlight_style(hl);
    f.render_stateful_widget(links, right, &mut app.link);

    // 按月看时，底部状态行也显示选中页面的时长

    let text = match app.current() {
        _ if app.input == Input::Filter => format!("/{}▏  (Enter 确认  Esc 清除)", app.query),
        _ if app.input == Input::Ask => format!("?{}▏  (Enter 问 jev  Esc 取消)", app.ask),
        Some(l) if app.on_links => format!("{} {}  |  {}  |  {}", l.time, history::dur(l.secs), l.url, app.status),
        _ => app.status.clone(),
    };
    let text = if app.input == Input::None && !app.query.is_empty() {
        format!("[/{}] {text}", app.query)
    } else {
        text
    };
    f.render_widget(Paragraph::new(text).style(Style::default().fg(Color::DarkGray)), foot);
}

pub fn run(
    mut cfg: Config,
    path: &Path,
    days: Days,
    searches: Searches,
) -> Result<(), Box<dyn Error>> {
    let mut app = App {
        all: days.into_iter().rev().collect(),
        days: Vec::new(),
        rows: Vec::new(),
        view: Vec::new(),
        query: String::new(),
        ask: String::new(),
        input: Input::None,
        scores: HashMap::new(),
        row: ListState::default(),
        link: ListState::default(),
        on_links: false,
        searches,
        sort: Sort::Time,
        show_time: false,
        panel: Panel::Searches,
        status: "j/k 移动  h/l 切换  / 搜索  ? jev  s 排序  T 时刻  S 域名合计  Enter 打开  x 排除该主机  q 退出".into(),
    };
    app.refilter();

    let mut term = ratatui::init();
    let result = (|| -> Result<(), Box<dyn Error>> {
        loop {
            term.draw(|f| draw(f, &mut app))?;

            let Event::Key(key) = event::read()? else { continue };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if app.input != Input::None {
                let filter = app.input == Input::Filter;
                let buf = if filter { &mut app.query } else { &mut app.ask };
                match key.code {
                    KeyCode::Enter => app.input = Input::None,
                    KeyCode::Esc => {
                        app.input = Input::None;
                        buf.clear();
                    }
                    KeyCode::Backspace => {
                        buf.pop();
                    }
                    KeyCode::Char(c) => buf.push(c),
                    // 输入时也能移动（j/k 会作为字符输入，所以用方向键）
                    KeyCode::Down | KeyCode::Up => {
                        app.step(key.code == KeyCode::Down);
                        continue;
                    }
                    KeyCode::Tab => {
                        app.on_links = !app.on_links;
                        continue;
                    }
                    _ => continue,
                }
                if filter {
                    app.refilter();
                } else if key.code == KeyCode::Enter && !app.ask.trim().is_empty() {
                    // ponytail: 等待期间界面会卡住。嫌慢的话再放到线程里
                    app.status = format!("正在问 jev…（{} 个页面）", app.view.len());
                    term.draw(|f| draw(f, &mut app))?;
                    app.rank(cfg.min_score);
                }
                continue;
            }
            match key.code {
                KeyCode::Char('/') => app.input = Input::Filter,
                KeyCode::Char('?') => {
                    app.input = Input::Ask;
                    app.ask.clear();
                }
                // 筛选状态下的 Esc 只清除搜索
                KeyCode::Esc if !app.query.is_empty() || !app.scores.is_empty() => {
                    app.query.clear();
                    app.refilter();
                }
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('j') | KeyCode::Down => app.step(true),
                KeyCode::Char('k') | KeyCode::Up => app.step(false),
                KeyCode::Char('h') | KeyCode::Left => app.on_links = false,
                KeyCode::Char('l') | KeyCode::Right => app.on_links = true,
                KeyCode::Tab => app.on_links = !app.on_links,
                KeyCode::Enter if !app.on_links => app.on_links = true,
                KeyCode::Enter => {
                    if let Some(url) = app.current().map(|l| l.url.clone()) {
                        app.status = match open(&url) {
                            Ok(()) => "已打开".into(),
                            Err(e) => format!("打不开: {e}"),
                        };
                    }
                }
                // 排序方式：时间顺序 → 按时长 → 按域名，循环
                KeyCode::Char('s') => {
                    app.sort = app.sort.next();
                    app.scores.clear();
                    app.sort_view();
                    app.link.select((!app.view.is_empty()).then_some(0));
                    app.status = format!("排序：{}", app.sort.label());
                }
                KeyCode::Char('T') => {
                    app.show_time = !app.show_time;
                    app.status = if app.show_time { "显示打开时刻".into() } else { "隐藏打开时刻".into() };
                }
                KeyCode::Char('S') => {
                    app.panel = if app.panel == Panel::Searches { Panel::Hosts } else { Panel::Searches };
                }
                KeyCode::Char('x') if app.on_links => {
                    let Some(host) = app.current().map(|l| l.host.clone()) else { continue };
                    config::add_deny(path, &host)?;
                    cfg.deny.push(host.clone());
                    for (_, links) in &mut app.all {
                        links.retain(|l| cfg.allowed(&l.host));
                    }
                    app.all.retain(|(_, l)| !l.is_empty());
                    app.refilter();
                    app.status = format!("已把 {host} 加入 deny");
                }
                _ => {}
            }
        }
    })();
    ratatui::restore();
    result
}