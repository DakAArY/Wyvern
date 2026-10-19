use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use lsp_types::CompletionItemKind;
use std::cmp::Reverse;

/// Calidad de la coincidencia, de mejor a peor, como en los IDEs de JetBrains:
/// lo escrito tal cual gana a camelHumps, y éste a una subsecuencia cualquiera.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier { Fuzzy, Substring, CamelHumps, PrefixIgnoreCase, Prefix, Exact }

pub struct Match {
    pub tier: Tier,
    pub fuzzy: i64,
    /// Índices (en chars) de `candidate` que coinciden, para resaltarlos.
    pub indices: Vec<usize>,
}

/// `None` si `typed` no es ni siquiera subsecuencia de `candidate`.
pub fn match_candidate(matcher: &SkimMatcherV2, typed: &str, candidate: &str) -> Option<Match> {
    if typed.is_empty() {
        return Some(Match { tier: Tier::Prefix, fuzzy: 0, indices: Vec::new() });
    }
    let (fuzzy, indices) = matcher.fuzzy_indices(candidate, typed)?;
    let lower = |s: &str| s.to_lowercase();
    let n = typed.chars().count();
    let tier = if candidate == typed {
        Tier::Exact
    } else if candidate.starts_with(typed) {
        Tier::Prefix
    } else if lower(candidate).starts_with(&lower(typed)) {
        Tier::PrefixIgnoreCase
    } else if camel_humps(&typed.chars().collect::<Vec<_>>(), &candidate.chars().collect::<Vec<_>>()) {
        Tier::CamelHumps
    } else if lower(candidate).contains(&lower(typed)) {
        Tier::Substring
    } else {
        Tier::Fuzzy
    };
    let indices = if tier >= Tier::PrefixIgnoreCase { (0..n).collect() } else { indices };
    Some(Match { tier, fuzzy, indices })
}

/// Clave de orden: calidad, uso previo, puntuación difusa (solo desempata entre
/// coincidencias débiles), tipo, `sortText` del servidor y longitud.
pub fn sort_key<'a>(m: &Match, usage: u32, kind: Option<CompletionItemKind>, sort_text: &'a str, label: &'a str)
    -> impl Ord + use<'a>
{
    let fuzzy = if m.tier <= Tier::Substring { m.fuzzy } else { 0 };
    let kind_rank = match kind {
        Some(CompletionItemKind::TEXT) => 2,
        Some(CompletionItemKind::SNIPPET) => 1,
        _ => 0,
    };
    (Reverse(m.tier), Reverse(usage.min(10)), Reverse(fuzzy), kind_rank, sort_text, label.len(), label)
}

/// "gCP", "gcp" o "CuPo" encajan con "getCurrentPosition": cada carácter
/// continúa el fragmento actual o abre uno nuevo (mayúscula, o tras `_`, `-`, `.`).
fn camel_humps(pat: &[char], word: &[char]) -> bool {
    fn boundary(w: &[char], j: usize) -> bool {
        j == 0 || (w[j].is_uppercase() && !w[j - 1].is_uppercase()) || (!w[j - 1].is_alphanumeric() && w[j].is_alphanumeric())
    }
    fn eq(a: char, b: char) -> bool { a.to_lowercase().eq(b.to_lowercase()) }
    // `dead` memoriza estados ya fallidos: sin ello el backtracking es exponencial.
    fn go(p: &[char], w: &[char], k: usize, i: usize, dead: &mut [bool]) -> bool {
        if k == p.len() { return true; }
        let key = k * (w.len() + 1) + i;
        if dead[key] { return false; }
        let ok = (i < w.len() && i > 0 && eq(w[i], p[k]) && go(p, w, k + 1, i + 1, dead))
            || (i..w.len()).any(|j| boundary(w, j) && eq(w[j], p[k]) && go(p, w, k + 1, j + 1, dead));
        if !ok { dead[key] = true; }
        ok
    }
    let mut dead = vec![false; (pat.len() + 1) * (word.len() + 1)];
    go(pat, word, 0, 0, &mut dead)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tier(typed: &str, cand: &str) -> Option<Tier> {
        match_candidate(&SkimMatcherV2::default().ignore_case(), typed, cand).map(|m| m.tier)
    }

    #[test]
    fn tiers_follow_jetbrains_priority() {
        assert_eq!(tier("foo", "foo"), Some(Tier::Exact));
        assert_eq!(tier("Hash", "HashMap"), Some(Tier::Prefix));
        assert_eq!(tier("hash", "HashMap"), Some(Tier::PrefixIgnoreCase));
        assert_eq!(tier("gcp", "getCurrentPosition"), Some(Tier::CamelHumps));
        assert_eq!(tier("CuPo", "getCurrentPosition"), Some(Tier::CamelHumps));
        assert_eq!(tier("tw", "to_writer"), Some(Tier::CamelHumps));
        assert_eq!(tier("map", "HashMap"), Some(Tier::CamelHumps));
        assert_eq!(tier("ashm", "HashMap"), Some(Tier::Substring));
        assert_eq!(tier("gtx", "getX"), Some(Tier::Fuzzy));
        assert_eq!(tier("zz", "getX"), None);
        assert_eq!(tier("", "anything"), Some(Tier::Prefix));
    }

    #[test]
    fn camel_humps_needs_segment_starts_and_is_not_exponential() {
        let c = |p: &str, w: &str| camel_humps(&p.chars().collect::<Vec<_>>(), &w.chars().collect::<Vec<_>>());
        assert!(c("ge", "getX"));
        assert!(!c("et", "getX"), "no puede empezar a mitad de fragmento");
        assert!(c("baCo", "background-color"));
        assert!(!c("bgCo", "background-color"));
        let word = "a_".repeat(200);
        assert!(!c(&format!("{}b", "a".repeat(30)), &word));
    }

    #[test]
    fn ranking_order() {
        let m = SkimMatcherV2::default().ignore_case();
        let mut items = vec![
            ("getX", None, "1"),
            ("HashMapExt", None, "0"),
            ("hashmap", Some(CompletionItemKind::TEXT), "0"),
            ("HashMap", None, "2"),
            ("SomeHashMap", None, "0"),
        ];
        items.retain(|(l, _, _)| match_candidate(&m, "Hash", l).is_some());
        items.sort_by_key(|(l, k, s)| sort_key(&match_candidate(&m, "Hash", l).unwrap(), 0, *k, s, l));
        let labels: Vec<_> = items.iter().map(|i| i.0).collect();
        assert_eq!(labels, ["HashMapExt", "HashMap", "hashmap", "SomeHashMap"]);

        // Lo ya elegido antes sube dentro de su misma calidad de coincidencia.
        let a = match_candidate(&m, "Hash", "HashMap").unwrap();
        let b = match_candidate(&m, "Hash", "HashMapExt").unwrap();
        assert!(sort_key(&a, 3, None, "2", "HashMap") < sort_key(&b, 0, None, "0", "HashMapExt"));
    }
}
