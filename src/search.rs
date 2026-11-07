use crate::apps::Application;

pub fn search(apps: &[Application], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    let terms: Vec<_> = query.split_whitespace().collect();
    let mut matches: Vec<_> = apps
        .iter()
        .enumerate()
        .filter_map(|(index, app)| {
            let name = app.name.to_lowercase();
            let haystack = format!(
                "{} {} {}",
                name,
                app.keywords.to_lowercase(),
                app.detail.to_lowercase()
            );
            if !terms.iter().all(|term| haystack.contains(term)) {
                return None;
            }
            let rank = if query.is_empty() || name == query {
                0
            } else if name.starts_with(&query) {
                1
            } else if name.contains(&query) {
                2
            } else {
                3
            };
            Some((rank, index))
        })
        .collect();
    // Stable sorting preserves the catalog's alphabetical order within a rank.
    matches.sort_by_key(|(rank, _)| *rank);
    matches.into_iter().map(|(_, index)| index).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::LaunchTarget;

    fn app(name: &str, keywords: &str) -> Application {
        Application {
            name: name.into(),
            detail: String::new(),
            keywords: keywords.into(),
            target: LaunchTarget::File("/unused".into()),
        }
    }

    #[test]
    fn ranks_exact_prefix_substring_and_keyword_matches() {
        let apps = vec![
            app("My Code", ""),
            app("Editor", "code"),
            app("Code Studio", ""),
            app("Code", ""),
        ];
        assert_eq!(search(&apps, " CODE "), vec![3, 2, 0, 1]);
    }

    #[test]
    fn matches_unicode_and_all_words() {
        let apps = vec![
            app("系统设置", "preferences system"),
            app("终端", "terminal"),
        ];
        assert_eq!(search(&apps, "设置 system"), vec![0]);
        assert!(search(&apps, "设置 terminal").is_empty());
        assert_eq!(search(&apps, "  "), vec![0, 1]);
        assert!(search(&[], "anything").is_empty());
    }
}
