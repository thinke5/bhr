# bhr — browser history reading

[English](README.md) · [日本語](README.ja.md)

把 Chrome 的浏览记录整理成「按天列出、你实际读过的页面」。
工作工具、搜索页、社交网站时间线、localhost 之类的内容由配置文件过滤掉，
同一个页面打开多少次也只合并成一行，并标出当天在该网址上停留的时间。

把 `chrome_root` 指向对应浏览器的配置文件目录，
Chromium 系浏览器（Brave、Edge、Vivaldi、Arc）也能读取。

## 安装

```sh
cargo install bhr
# 或者从 clone 下来的目录
cargo install --path .
```

## 用法

```sh
bhr                                # TUI（默认）
bhr list --day 2026-09-28          # 一天的记录，Markdown（`list` 是 `report` 的别名）
bhr list --since 2026-09-01        # 一段时期（也可用 --until）
bhr list --json --since 2026-09-01 # [{"date", "links": [{"host", "title", "url", "secs"}]}]
bhr list --since 2026-09-01 --ask "Rust 内存管理"  # 用 Jev 给每天的页面打分排序
bhr searches --day 2026-09-28      # 当天搜索过的词，以及从搜索结果打开的页面
bhr searches --json                # [{"date", "searches": [{"term", "links": [{"host", "title", "url", "secs"}]}]}]
bhr hosts --top 50                 # 按条数列出主机——用来找要排除的
bhr hosts --json                   # [{"host", "count", "minutes"}]
bhr config                         # 显示配置文件路径
```

`--config`、`--since`、`--until`、`--json` 对每个子命令都可用。

### 使用时间（30s / 15min 20s / 1h 18min 3s）

每个网址都会标出当天在上面停留的时间，精确到秒：`30s`、`15min 20s`、`1h 18min 3s`，
四舍五入到秒，基本没停留的记为 `<1s`：

- 时间来自 Chrome 的 `visit_duration`（微秒），按 **网址（URL）** 累加：同一天同一网址访问几次就相加，
  不同网址各自计算，不会混在一起。
- 同一天里，主机和标题相同的页面仍然只留一行（取最短的 URL），该行的时长就是 URL 的时长。
- 单次访问的时长按 `max_visit_min`（默认 60 分钟）封顶。页面长时间挂在标签页上不关时，
  Chrome 会记下以小时甚至天计的时长，不封顶的话数字会被拉爆。
- `list` 默认按时间顺序排（当天第一次打开的先后），加 `--dur` 显示时长、`--time` 显示打开时刻（`H:MM:SS`）。
  `--sort duration` 按时长从长到短，`--sort host` 按域名分组，分组标题下每行带时长。`--ask` 时按相关度排序，行尾附时长。
- 同一天打开过多次的页面，时刻取第一次打开的时间。
- TUI 右侧列表最左一列是时长，底部状态行也显示当前页面的时刻和时长。
  按键：`s` 循环切换排序（时间顺序 → 按时长 → 按域名），`T` 显示/隐藏打开时刻，
  `S` 把右下角框在「搜索词」和「域名合计」（每个域名的页数、合计时长）之间切换。
- `hosts` 会额外输出每个主机的合计时长（`duration` 文本 + `secs` 秒数，`minutes` 仍保留）。

### 从命令行搜索

文字搜索不需要额外东西——Markdown 里一个页面一行，可以直接 grep；JSON 可以用 `jq`：

```sh
bhr list --since 2026-09-01 | grep -i rust
bhr list --json --since 2026-09-01 | jq '[.[] | .links |= map(select(.title | test("rust"; "i")))]'
```

要按语义搜索请用 `--ask`（见 [语义搜索](#语义搜索)）。每天按相关度排序，
JSON 里每个链接会多一个 `score`（0–1）。配置里 `min_score`（默认 `0.7`）
以下的页面会被去掉，`--ask` 和 TUI 里的 `?` 都是如此。

把 `bhr list` 的输出通过管道交给 `jev` CLI 不能实现这个用途：
`jev` 会把整个标准输入当成一个状态、只返回一个答案，而不是每页一个分数。

### 搜索词

`searches` 读取你在搜索框里输入过的词（Chrome 的 `keyword_search_terms`），
每天每个词只出现一次，按第一次搜索的顺序排列。每个词下面是
从该搜索结果页打开的页面（沿 `from_visit` 追一跳，经过跳转的页面可能漏掉）。
搜索词本身不做过滤，但打开的页面和 `list` 用同一套过滤，
所以点到被排除的主机（比如 Amazon）和首页的不会显示。

TUI 在页面列表下方显示所选月份或日期的搜索词，以及从每个词打开的主机（最多 8 行，不能滚动）。

### TUI 按键

| 按键 | 动作 |
|---|---|
| `j` / `k`, `↓` / `↑` | 移动 |
| `h` / `l`, `Tab` | 在日期和页面之间切换 |
| `/` | 按文字筛选（空白分开的词都要出现在标题、主机或 URL 中；`Esc` 清除）。输入时 `↑`/`↓`/`Tab` 仍可移动 |
| `?` | 用 [jev](https://github.com/polidog/jev) 语义搜索：把当前日期或月份的页面按相关度排序 |
| `Enter` | 在浏览器中打开页面（`xdg-open` / `open`） |
| `x` | 把选中页面的主机加入 `deny`（写入配置文件） |
| `q` / `Esc` | 退出 |

### 语义搜索

TUI 里的 `?`（选中的日期）和 `list --ask`（区间内每一天）机制相同。
每个页面（标题、主机、URL）都会连同一个是/否问题发给 Jev——
「这是搜索 *query* 的人想要的页面吗？」——列表按该概率排序。可以先配合 `/` 缩小页面范围。

一个页面一次请求，8 个并行；TUI 会等到所有答案返回。
使用 `--ask` 时请用 `--since` / `--day` 缩小范围——一个月的记录可能有数千次请求。
服务商与 `jev` CLI 一致：`JEV_PROVIDER` = `typesafe`（默认，需要
`TYPESAFE_API_KEY`）、`cloudflare`（`CLOUDFLARE_ACCOUNT_ID`、`CLOUDFLARE_API_TOKEN`）
或 `vercel`（`AI_GATEWAY_API_KEY`）。

注意：这会把页面标题和 URL 发送给该服务商。

## 配置

`~/.config/bhr/config.toml`（会遵循 `$XDG_CONFIG_HOME`）。首次运行时按默认内容创建。

```toml
chrome_root = ""     # 空 = 系统默认（~/.config/google-chrome、~/Library/Application Support/Google/Chrome）
profiles = []        # 空 = Default 和所有 "Profile N"
allow = []           # 空 = 除 deny 外全部；否则只收录这些主机
deny = ["github.com", "x.com", "google.com"]   # 要丢弃的主机，含子域名
tracking_params = ["utm_*", "fbclid", "ref"]   # 要去掉的查询参数；末尾 * 表示前缀
max_visit_min = 60   # 单次访问最多记录多少分钟（0 = 不封顶）
```

不论配置如何，以下内容总会被丢弃：localhost、IP 地址、`.local` / `.test` /
`.internal` 主机、认证页面（`auth.`、`login.`、`accounts.` …）、首页（`/`）、
iframe 访问。

提示：只用 deny 模式会让工作域名也进来。如果只想保留阅读内容，
就把你阅读的网站列在 `allow` 里。

## 实现方式

- `History` SQLite 文件会先复制到临时目录再打开（Chrome 运行时它会锁住文件）。
- 日期使用本地时间。
- 同一天内，页面按主机 + 标题去重，保留最短的 URL；访问时长按 URL 汇总。
- Chrome 会删除约 90 天以前的记录，想保留的话请定期导出。
