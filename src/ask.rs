//! 问 jev「这个页面符合搜索要求吗」。
use std::error::Error;

use clap::ValueEnum;
use indexmap::IndexMap;
use jev::cli::ProviderKind;
use jev::model::{Answer, Question, Request};
use serde_json::json;

use crate::history::Link;

/// 同时发出的请求数
const PARALLEL: usize = 8;

/// 与 jev CLI 一样用 `JEV_PROVIDER` 选择（默认 typesafe）。
fn provider() -> Result<ProviderKind, Box<dyn Error>> {
    match std::env::var("JEV_PROVIDER") {
        Ok(name) => Ok(ProviderKind::from_str(&name, true)?),
        Err(_) => Ok(ProviderKind::Typesafe),
    }
}

/// 每个页面返回「符合搜索」的概率（0〜1）。顺序与 `links` 相同。
pub fn relevance(links: &[Link], query: &str) -> Result<Vec<f64>, Box<dyn Error>> {
    let kind = provider()?;
    let ask = move |link: &Link| -> Result<f64, String> {
        let req = Request {
            state: json!({ "title": link.title, "host": link.host, "url": link.url }),
            questions: IndexMap::from([(
                "match".to_string(),
                Question::Noul {
                    instructions: json!(format!("Is this web page what someone searching for \"{query}\" is looking for?")),
                    criteria: None,
                },
            )]),
        };
        match jev::provider::of(kind).evaluate(&req).map_err(|e| e.to_string())?.answers.get("match") {
            Some(Answer::Noul { noul }) => Ok(*noul),
            _ => Err("jev 的回答里没有 noul".into()),
        }
    };
    // ponytail: 一页一次请求。页面多到几百以上、嫌慢的话，再把问题合进一次请求
    let chunk = std::cmp::max(links.len().div_ceil(PARALLEL), 1);
    let scores: Vec<Result<Vec<f64>, String>> = std::thread::scope(|s| {
        let handles: Vec<_> = links
            .chunks(chunk)
            .map(|part| s.spawn(move || part.iter().map(ask).collect::<Result<Vec<_>, _>>()))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("jev 线程崩溃了".into()))).collect()
    });
    let mut out = Vec::with_capacity(links.len());
    for part in scores {
        out.extend(part?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 调用真实 API。`cargo test -- --ignored`（需要 TYPESAFE_API_KEY）
    #[test]
    #[ignore]
    fn ranks_by_meaning() {
        let link = |title: &str, host: &str| Link {
            url: format!("https://{host}/a"),
            host: host.into(),
            title: title.into(),
            secs: 0,
            time: String::new(),
            order: 0,
        };
        let links = [
            link("图解理解所有权与借用", "zenn.dev"),
            link("今日晚饭菜谱 10 选", "cookpad.com"),
            link("Understanding lifetimes in Rust", "blog.example.com"),
        ];
        let s = relevance(&links, "Rust 的内存管理").unwrap();
        println!("{s:?}");
        assert!(s[0] > s[1] && s[2] > s[1]);
    }
}
