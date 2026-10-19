//! Ports of the PHP engine functions whose exact behavior Composer depends on.

use std::cmp::Ordering::{self, Equal, Greater, Less};

/// PHP's `version_compare($a, $b)` (php-src ext/standard/versioning.c `php_version_compare`).
pub fn version_compare(a: &str, b: &str) -> Ordering {
    numeric_compare(a.as_bytes(), b.as_bytes()).unwrap_or_else(|| compare(c_str(a), c_str(b)))
}

/// [`compare`] of two plain `1.2.3.4` versions (most normalized ones), which canonicalizing leaves as they are.
fn numeric_compare(a: &[u8], b: &[u8]) -> Option<Ordering> {
    let plain = |s: &[u8]| !s.is_empty() && s.split(|&c| c == b'.').all(|part| !part.is_empty() && part.iter().all(u8::is_ascii_digit));
    if !plain(a) || !plain(b) {
        return None;
    }
    let (mut a, mut b) = (a.split(|&c| c == b'.'), b.split(|&c| c == b'.'));
    loop {
        match (a.next(), b.next()) {
            (Some(x), Some(y)) if strtol(x) == strtol(y) => {}
            (Some(x), Some(y)) => return Some(strtol(x).cmp(&strtol(y))),
            // a part left over makes a version greater
            (Some(_), None) => return Some(Greater),
            (None, Some(_)) => return Some(Less),
            (None, None) => return Some(Equal),
        }
    }
}

/// The C code sees NUL-terminated strings.
fn c_str(s: &str) -> &[u8] {
    s.as_bytes().split(|&c| c == 0).next().unwrap_or_default()
}

pub(crate) fn compare(v1: &[u8], v2: &[u8]) -> Ordering {
    if v1.is_empty() || v2.is_empty() {
        return (!v1.is_empty()).cmp(&!v2.is_empty());
    }
    let (mut stack1, mut stack2) = ([0; 64], [0; 64]);
    let (mut heap1, mut heap2) = (Vec::new(), Vec::new());
    let s1 = if v1[0] == b'#' { v1 } else { canonicalize(v1, &mut stack1, &mut heap1) };
    let s2 = if v2[0] == b'#' { v2 } else { canonicalize(v2, &mut stack2, &mut heap2) };

    let next_dot = |s: &[u8], from: usize| s[from..].iter().position(|&c| c == b'.').map(|i| from + i);
    let (mut p1, mut p2) = (0, 0);
    // `n1`/`n2` are the C pointers to the next '.', non-NULL before the first split.
    let (mut n1, mut n2) = (Some(0), Some(0));
    let mut cmp = Equal;
    while p1 < s1.len() && p2 < s2.len() && n1.is_some() && n2.is_some() {
        n1 = next_dot(s1, p1);
        n2 = next_dot(s2, p2);
        let part1 = &s1[p1..n1.unwrap_or(s1.len())];
        let part2 = &s2[p2..n2.unwrap_or(s2.len())];
        cmp = match (starts_with_digit(part1), starts_with_digit(part2)) {
            (true, true) => strtol(part1).cmp(&strtol(part2)),
            (false, false) => special_form(part1).cmp(&special_form(part2)),
            (true, false) => special_form(b"#N#").cmp(&special_form(part2)),
            (false, true) => special_form(part1).cmp(&special_form(b"#N#")),
        };
        if cmp != Equal {
            break;
        }
        if let Some(n) = n1 {
            p1 = n + 1;
        }
        if let Some(n) = n2 {
            p2 = n + 1;
        }
    }
    if cmp == Equal {
        if n1.is_some() {
            cmp = if starts_with_digit(&s1[p1..]) { Greater } else { compare(&s1[p1..], b"#N#") };
        } else if n2.is_some() {
            cmp = if starts_with_digit(&s2[p2..]) { Less } else { compare(b"#N#", &s2[p2..]) };
        }
    }
    cmp
}

fn starts_with_digit(s: &[u8]) -> bool {
    s.first().is_some_and(u8::is_ascii_digit)
}

/// `strtol()` on a part that starts with a digit; saturates at `LONG_MAX` like the C function.
fn strtol(s: &[u8]) -> i64 {
    s.iter().take_while(|c| c.is_ascii_digit()).fold(0i64, |n, &c| n.saturating_mul(10).saturating_add(i64::from(c - b'0')))
}

/// `compare_special_version_forms()`: prefix match (`strncmp`) against the known forms, unknown = -1.
fn special_form(form: &[u8]) -> i8 {
    const FORMS: [(&[u8], i8); 10] =
        [(b"dev", 0), (b"alpha", 1), (b"a", 1), (b"beta", 2), (b"b", 2), (b"RC", 3), (b"rc", 3), (b"#", 4), (b"pl", 5), (b"p", 5)];
    FORMS.iter().find(|(name, _)| form.starts_with(name)).map_or(-1, |&(_, order)| order)
}

/// `php_canonicalize_version()`: `.` between digit and non-digit runs, other separators become `.`,
/// the first byte is copied verbatim. Uses `stack` when the result fits so the hot path does not allocate.
fn canonicalize<'a>(v: &[u8], stack: &'a mut [u8; 64], heap: &'a mut Vec<u8>) -> &'a [u8] {
    let buf: &mut [u8] = if 2 * v.len() <= stack.len() {
        stack
    } else {
        heap.resize(2 * v.len(), 0);
        heap
    };
    let isdig = |c: u8| c.is_ascii_digit();
    let isndig = |c: u8| !c.is_ascii_digit() && c != b'.';
    buf[0] = v[0];
    let mut q = 1;
    let mut lp = v[0];
    for &c in &v[1..] {
        let special = matches!(c, b'-' | b'_' | b'+');
        let transition = !special && (isndig(lp) && isdig(c) || isdig(lp) && isndig(c));
        let keep = transition || (!special && c.is_ascii_alphanumeric());
        if (!keep || transition) && buf[q - 1] != b'.' {
            buf[q] = b'.';
            q += 1;
        }
        if keep {
            buf[q] = c;
            q += 1;
        }
        lp = c;
    }
    if buf[q - 1] == b'.' {
        q -= 1;
    }
    &buf[..q]
}

/// PHP 8 `$a == $b` on two strings (`zendi_smart_streq`): numeric strings compare as numbers.
pub(crate) fn loose_eq(a: &str, b: &str) -> bool {
    let (Some((l1, d1, o1)), Some((l2, d2, o2))) = (numeric(a), numeric(b)) else { return a == b };
    match (l1, l2) {
        (Some(x), Some(y)) => x == y,
        // integers overflown to the same side (or infinities) compare as strings
        _ if d1 == d2 && (o1 != 0 && o1 == o2 || d1.is_infinite()) => a == b,
        (Some(_), None) if o2 != 0 => false,
        (None, Some(_)) if o1 != 0 => false,
        _ => d1 == d2,
    }
}

/// `is_numeric_string_ex()`: `(long value if the string is an in-range integer, double value, integer overflow side)`.
fn numeric(s: &str) -> Option<(Option<i64>, f64, i8)> {
    let ws = [' ', '\t', '\n', '\r', '\x0B', '\x0C'];
    let t = s.trim_start_matches(ws).trim_end_matches(ws);
    // Rust's float grammar matches PHP's once `inf`/`nan` are ruled out.
    if !t.bytes().all(|c| c.is_ascii_digit() || b"+-.eE".contains(&c)) {
        return None;
    }
    let double: f64 = t.parse().ok()?;
    let is_int = !t.contains(['.', 'e', 'E']);
    let long = if is_int { t.parse().ok() } else { None };
    let overflow = match (is_int && long.is_none(), t.starts_with('-')) {
        (false, _) => 0,
        (true, true) => -1,
        (true, false) => 1,
    };
    Some((long, double, overflow))
}

/// `(string) ($numericString + $int)`: past `PHP_INT_MAX` PHP switches to a float, printed with `precision=14`.
pub(crate) fn add(s: &str, n: i64) -> String {
    if let Some(sum) = s.parse::<i64>().ok().and_then(|v| v.checked_add(n)) {
        return sum.to_string();
    }
    // `%.14G`, always in exponent form at this magnitude: 1.0E+20, 9.2233720368548E+18
    let e = format!("{:.13e}", s.parse::<f64>().unwrap_or_default() + n as f64);
    let (mantissa, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let mantissa = mantissa.trim_end_matches('0');
    let zero = if mantissa.ends_with('.') { "0" } else { "" };
    let sign = if exp.starts_with('-') { "" } else { "+" };
    format!("{mantissa}{zero}E{sign}{exp}")
}

/// PHP 8's `usort()`: `zend_sort()` with the original-order tiebreak that makes it stable.
/// Returns the sorted order as indices into `v`. Ported rather than using `slice::sort_by` because
/// Composer's comparators are not always consistent (two dev branches each sort after the other),
/// which makes the result algorithm-dependent.
pub(crate) fn usort<T>(v: &[T], mut cmp: impl FnMut(&T, &T) -> Ordering) -> Vec<usize> {
    let mut order: Vec<usize> = (0..v.len()).collect();
    zend_sort(&mut order, &mut |a, b| cmp(&v[a], &v[b]).then(a.cmp(&b)) == Greater);
    order
}

type Gt<'a> = dyn FnMut(usize, usize) -> bool + 'a;

/// `zend_sort()` (Zend/zend_sort.c): insertion sort up to 16 elements, quicksort above.
fn zend_sort(mut v: &mut [usize], gt: &mut Gt) {
    loop {
        let n = v.len();
        if n <= 16 {
            return insert_sort(v, gt);
        }
        let offset = n >> 1;
        if n >> 10 != 0 {
            let delta = offset >> 1;
            sort5(v, [0, delta, offset, offset + delta, n - 1], gt);
        } else {
            sort3(v, [0, offset, n - 1], gt);
        }
        v.swap(1, offset);
        let pivot = 1;
        let (mut i, mut j) = (pivot + 1, n - 1);
        'done: loop {
            while gt(v[pivot], v[i]) {
                i += 1;
                if i == j {
                    break 'done;
                }
            }
            j -= 1;
            if j == i {
                break 'done;
            }
            while gt(v[j], v[pivot]) {
                j -= 1;
                if j == i {
                    break 'done;
                }
            }
            v.swap(i, j);
            i += 1;
            if i == j {
                break 'done;
            }
        }
        v.swap(pivot, i - 1);
        let (left, right) = std::mem::take(&mut v).split_at_mut(i);
        let left = &mut left[..i - 1];
        if left.len() < right.len() {
            zend_sort(left, gt);
            v = right;
        } else {
            zend_sort(right, gt);
            v = left;
        }
    }
}

/// `zend_insert_sort()`.
fn insert_sort(v: &mut [usize], gt: &mut Gt) {
    let n = v.len();
    match n {
        0 | 1 => {}
        2 => sink(v, &[0, 1], gt),
        3 => sort3(v, [0, 1, 2], gt),
        4 => sort4(v, [0, 1, 2, 3], gt),
        5 => sort5(v, [0, 1, 2, 3, 4], gt),
        _ => {
            for i in 1..6 {
                let mut j = i - 1;
                if !gt(v[j], v[i]) {
                    continue;
                }
                while j != 0 {
                    j -= 1;
                    if !gt(v[j], v[i]) {
                        j += 1;
                        break;
                    }
                }
                v[j..=i].rotate_right(1);
            }
            for i in 6..n {
                let mut j = i - 1;
                if !gt(v[j], v[i]) {
                    continue;
                }
                loop {
                    j -= 2;
                    if !gt(v[j], v[i]) {
                        j += 1;
                        if !gt(v[j], v[i]) {
                            j += 1;
                        }
                        break;
                    }
                    if j == 0 {
                        break;
                    }
                    if j == 1 {
                        j -= 1;
                        if gt(v[i], v[j]) {
                            j += 1;
                        }
                        break;
                    }
                }
                v[j..=i].rotate_right(1);
            }
        }
    }
}

/// `zend_sort_3()`.
fn sort3(v: &mut [usize], [a, b, c]: [usize; 3], gt: &mut Gt) {
    if !gt(v[a], v[b]) {
        if !gt(v[b], v[c]) {
            return;
        }
        v.swap(b, c);
        if gt(v[a], v[b]) {
            v.swap(a, b);
        }
        return;
    }
    if !gt(v[c], v[b]) {
        v.swap(a, c);
        return;
    }
    v.swap(a, b);
    if gt(v[b], v[c]) {
        v.swap(b, c);
    }
}

/// `zend_sort_4()`.
fn sort4(v: &mut [usize], idx: [usize; 4], gt: &mut Gt) {
    sort3(v, [idx[0], idx[1], idx[2]], gt);
    sink(v, &idx, gt);
}

/// `zend_sort_5()`.
fn sort5(v: &mut [usize], idx: [usize; 5], gt: &mut Gt) {
    sort4(v, [idx[0], idx[1], idx[2], idx[3]], gt);
    sink(v, &idx, gt);
}

/// The swap cascade `zend_sort_2/4/5` use to move the last element down into place.
fn sink(v: &mut [usize], idx: &[usize], gt: &mut Gt) {
    for k in (1..idx.len()).rev() {
        if !gt(v[idx[k - 1]], v[idx[k]]) {
            break;
        }
        v.swap(idx[k - 1], idx[k]);
    }
}
