//! Hunk-level three-way merge with `git merge-file`'s decisions (§5.12).
//!
//! The line diff is a self-contained linear-space Myers diff (the §9 choice
//! recorded in docs/impl-progress/gitio.md); the merge walks the two diff
//! scripts the way xdiff's `xdl_do_merge` does.

use std::collections::HashMap;

/// The result of [`merge3`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Merge3 {
    /// Every change merged; the merged bytes.
    Clean(Vec<u8>),
    /// At least one conflict region, in file order.
    Conflict(Vec<Hunk>),
}

/// One conflict region. Starts are 1-based line numbers; for an empty
/// section the start is the line after the insertion point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub base_start: usize,
    pub ours_start: usize,
    pub theirs_start: usize,
    pub base: Vec<u8>,
    pub ours: Vec<u8>,
    pub theirs: Vec<u8>,
}

/// git's binary rule: a NUL byte within the first 8000 bytes.
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(8000)].contains(&0)
}

/// Three-way merge of `ours` and `theirs` against `base`.
pub fn merge3(base: &[u8], ours: &[u8], theirs: &[u8]) -> Merge3 {
    if ours == theirs || theirs == base {
        return Merge3::Clean(ours.to_vec());
    }
    if ours == base {
        return Merge3::Clean(theirs.to_vec());
    }
    if is_binary(base) || is_binary(ours) || is_binary(theirs) {
        return whole_file(base, ours, theirs);
    }
    let (b, o, t) = (split_lines(base), split_lines(ours), split_lines(theirs));
    let mut ids: HashMap<&[u8], u32> = HashMap::new();
    let (bi, oi, ti) = (intern(&mut ids, &b), intern(&mut ids, &o), intern(&mut ids, &t));
    let d1 = diff(&bi, &oi);
    let d2 = diff(&bi, &ti);
    let Some(recs) = merge_scripts(&d1, &d2, bi.len(), oi.len(), ti.len(), &oi, &ti) else {
        tracing::warn!("merge3: inconsistent merge regions; reporting the whole file as one conflict");
        return whole_file(base, ours, theirs);
    };

    let cat = |ls: &[&[u8]], from: usize, n: usize| -> Vec<u8> { ls[from..from + n].concat() };
    let mut hunks = Vec::new();
    let mut out = Vec::with_capacity(ours.len().max(theirs.len()));
    let mut pos = 0;
    for r in &recs {
        out.extend(o[pos..r.i1].iter().flat_map(|l| l.iter()));
        match r.mode {
            Mode::Theirs => out.extend(cat(&t, r.i2, r.chg2)),
            Mode::Ours | Mode::Same => out.extend(cat(&o, r.i1, r.chg1)),
            Mode::Conflict => hunks.push(Hunk {
                base_start: r.i0 + 1,
                ours_start: r.i1 + 1,
                theirs_start: r.i2 + 1,
                base: cat(&b, r.i0, r.chg0),
                ours: cat(&o, r.i1, r.chg1),
                theirs: cat(&t, r.i2, r.chg2),
            }),
        }
        pos = r.i1 + r.chg1;
    }
    if !hunks.is_empty() {
        return Merge3::Conflict(hunks);
    }
    out.extend(o[pos..].iter().flat_map(|l| l.iter()));
    Merge3::Clean(out)
}

fn intern<'a>(ids: &mut HashMap<&'a [u8], u32>, lines: &[&'a [u8]]) -> Vec<u32> {
    lines
        .iter()
        .map(|l| {
            let n = ids.len() as u32;
            *ids.entry(*l).or_insert(n)
        })
        .collect()
}

fn whole_file(base: &[u8], ours: &[u8], theirs: &[u8]) -> Merge3 {
    Merge3::Conflict(vec![Hunk {
        base_start: 1,
        ours_start: 1,
        theirs_start: 1,
        base: base.to_vec(),
        ours: ours.to_vec(),
        theirs: theirs.to_vec(),
    }])
}

/// Lines keep their `\n` (a `\r` stays in the line); a last line without
/// `\n` counts.
fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.split_inclusive(|&c| c == b'\n').collect()
}

/// One change of a diff script: `chg1` lines at `i1` in the old file became
/// `chg2` lines at `i2` in the new one (0-based).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Change {
    pub i1: usize,
    pub chg1: usize,
    pub i2: usize,
    pub chg2: usize,
}

/// Minimal line diff (Myers, linear space), with each change group slid
/// down as far as it goes, then turned into a script.
pub(crate) fn diff(a: &[u32], b: &[u32]) -> Vec<Change> {
    let mut ra = vec![false; a.len()];
    let mut rb = vec![false; b.len()];
    let max = a.len() + b.len() + 2;
    let mut vf = vec![0isize; 2 * max + 3];
    let mut vb = vec![0isize; 2 * max + 3];
    compare(a, b, 0, a.len(), 0, b.len(), &mut ra, &mut rb, &mut vf, &mut vb);
    compact(a, &mut ra);
    compact(b, &mut rb);
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && !ra[i] && !rb[j] {
            i += 1;
            j += 1;
            continue;
        }
        let (si, sj) = (i, j);
        while i < a.len() && ra[i] {
            i += 1;
        }
        while j < b.len() && rb[j] {
            j += 1;
        }
        if i == si && j == sj {
            // Inconsistent marks cannot happen; never loop forever.
            debug_assert!(false, "inconsistent diff marks");
            break;
        }
        out.push(Change { i1: si, chg1: i - si, i2: sj, chg2: j - sj });
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn compare(
    a: &[u32],
    b: &[u32],
    mut a0: usize,
    mut a1: usize,
    mut b0: usize,
    mut b1: usize,
    ra: &mut [bool],
    rb: &mut [bool],
    vf: &mut [isize],
    vb: &mut [isize],
) {
    while a0 < a1 && b0 < b1 && a[a0] == b[b0] {
        a0 += 1;
        b0 += 1;
    }
    while a0 < a1 && b0 < b1 && a[a1 - 1] == b[b1 - 1] {
        a1 -= 1;
        b1 -= 1;
    }
    if a0 == a1 {
        rb[b0..b1].iter_mut().for_each(|m| *m = true);
        return;
    }
    if b0 == b1 {
        ra[a0..a1].iter_mut().for_each(|m| *m = true);
        return;
    }
    let (x, y) = middle_snake(&a[a0..a1], &b[b0..b1], vf, vb);
    compare(a, b, a0, a0 + x, b0, b0 + y, ra, rb, vf, vb);
    compare(a, b, a0 + x, a1, b0 + y, b1, ra, rb, vf, vb);
}

/// A point on an optimal edit path strictly between the two corners.
/// Both inputs are non-empty and differ in their first and last elements.
fn middle_snake(a: &[u32], b: &[u32], vf: &mut [isize], vb: &mut [isize]) -> (usize, usize) {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let delta = n - m;
    let odd = delta & 1 != 0;
    let off = (n + m + 1) as usize; // index of diagonal 0
    let dmax = (n + m + 1) / 2;
    // vf[k]: furthest x on diagonal k = x - y going forward.
    // vb[c]: furthest u on diagonal c = u - v going backward, u = n - x, v = m - y.
    vf[off + 1] = 0;
    vb[off + 1] = 0;
    let at = |k: isize| (off as isize + k) as usize;
    for d in 0..=dmax {
        let mut k = -d;
        while k <= d {
            let mut x = if k == -d || (k != d && vf[at(k - 1)] < vf[at(k + 1)]) {
                vf[at(k + 1)]
            } else {
                vf[at(k - 1)] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            vf[at(k)] = x;
            let c = delta - k;
            if odd && c >= -(d - 1) && c <= d - 1 && x + vb[at(c)] >= n {
                return (x as usize, y as usize);
            }
            k += 2;
        }
        let mut c = -d;
        while c <= d {
            let mut u = if c == -d || (c != d && vb[at(c - 1)] < vb[at(c + 1)]) {
                vb[at(c + 1)]
            } else {
                vb[at(c - 1)] + 1
            };
            let mut v = u - c;
            while u < n && v < m && a[(n - u - 1) as usize] == b[(m - v - 1) as usize] {
                u += 1;
                v += 1;
            }
            vb[at(c)] = u;
            let k = delta - c;
            if !odd && k >= -d && k <= d && vf[at(k)] + u >= n {
                return ((n - u) as usize, (m - v) as usize);
            }
            c += 2;
        }
    }
    unreachable!("Myers search ends within (n + m + 1) / 2 rounds")
}

/// Slide each group of changed lines down while the line after the group
/// equals its first line (xdiff's compaction direction); groups that meet
/// are joined.
fn compact(lines: &[u32], marks: &mut [bool]) {
    let n = lines.len();
    let mut i = 0;
    while i < n {
        if !marks[i] {
            i += 1;
            continue;
        }
        let mut s = i;
        let mut e = i;
        while e < n && marks[e] {
            e += 1;
        }
        while e < n && lines[s] == lines[e] {
            marks[s] = false;
            marks[e] = true;
            s += 1;
            e += 1;
            while e < n && marks[e] {
                e += 1;
            }
        }
        i = e;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Conflict,
    Ours,
    Theirs,
    /// A conflict region whose two sides came out identical: one copy.
    Same,
}

#[derive(Clone, Copy, Debug)]
struct Rec {
    mode: Mode,
    i0: usize,
    chg0: usize,
    i1: usize,
    chg1: usize,
    i2: usize,
    chg2: usize,
}

#[allow(clippy::too_many_arguments)]
fn append(recs: &mut Vec<Rec>, bad: &mut bool, mode: Mode, i0: usize, chg0: usize, i1: isize, chg1: usize, i2: isize, chg2: usize) {
    // Positions are signed as in xdiff: a leftover change after an
    // overlapping one can compute a side start before the previous region's
    // start; it is always folded into that region.
    if let Some(m) = recs.last_mut()
        && (i1 <= (m.i1 + m.chg1) as isize || i2 <= (m.i2 + m.chg2) as isize)
    {
        if mode != m.mode {
            m.mode = Mode::Conflict;
        }
        let ext = |end: isize, start: usize| usize::try_from(end - start as isize).ok();
        match (
            ext((i0 + chg0) as isize, m.i0),
            ext(i1 + chg1 as isize, m.i1),
            ext(i2 + chg2 as isize, m.i2),
        ) {
            (Some(c0), Some(c1), Some(c2)) => (m.chg0, m.chg1, m.chg2) = (c0, c1, c2),
            _ => *bad = true,
        }
        return;
    }
    let (Ok(i1), Ok(i2)) = (usize::try_from(i1), usize::try_from(i2)) else {
        *bad = true;
        return;
    };
    recs.push(Rec { mode, i0, chg0, i1, chg1, i2, chg2 });
}

/// xdiff's `xdl_do_merge` walk over the base→ours (`x1`) and base→theirs
/// (`x2`) scripts: changes whose base ranges overlap or touch form one
/// conflict region; a region whose two sides are identical takes one copy.
fn merge_scripts(
    x1: &[Change],
    x2: &[Change],
    nbase: usize,
    nours: usize,
    ntheirs: usize,
    ours: &[u32],
    theirs: &[u32],
) -> Option<Vec<Rec>> {
    let mut recs = Vec::new();
    let mut bad = false;
    let (mut p, mut q) = (0, 0);
    while p < x1.len() && q < x2.len() {
        let (c1, c2) = (x1[p], x2[q]);
        if c1.i1 + c1.chg1 < c2.i1 {
            let i2 = (c2.i2 + c1.i1) as isize - c2.i1 as isize;
            append(&mut recs, &mut bad, Mode::Ours, c1.i1, c1.chg1, c1.i2 as isize, c1.chg2, i2, c1.chg1);
            p += 1;
            continue;
        }
        if c2.i1 + c2.chg1 < c1.i1 {
            let i1 = (c1.i2 + c2.i1) as isize - c1.i1 as isize;
            append(&mut recs, &mut bad, Mode::Theirs, c2.i1, c2.chg1, i1, c2.chg1, c2.i2 as isize, c2.chg2);
            q += 1;
            continue;
        }
        let identical = c1.i1 == c2.i1
            && c1.chg1 == c2.chg1
            && c1.chg2 == c2.chg2
            && ours[c1.i2..c1.i2 + c1.chg2] == theirs[c2.i2..c2.i2 + c2.chg2];
        if !identical {
            // Signed arithmetic, as in xdiff.
            let off = c1.i1 as isize - c2.i1 as isize;
            let ffo = off + c1.chg1 as isize - c2.chg1 as isize;
            let (mut i0, mut i1, mut i2) = (c1.i1 as isize, c1.i2 as isize, c2.i2 as isize);
            if off > 0 {
                i0 -= off;
                i1 -= off;
            } else {
                i2 += off;
            }
            let mut chg0 = (c1.i1 + c1.chg1) as isize - i0;
            let mut chg1 = (c1.i2 + c1.chg2) as isize - i1;
            let mut chg2 = (c2.i2 + c2.chg2) as isize - i2;
            if ffo < 0 {
                chg0 -= ffo;
                chg1 -= ffo;
            } else {
                chg2 += ffo;
            }
            append(
                &mut recs,
                &mut bad,
                Mode::Conflict,
                i0 as usize,
                chg0 as usize,
                i1,
                chg1 as usize,
                i2,
                chg2 as usize,
            );
        }
        let e1 = c1.i1 + c1.chg1;
        let e2 = c2.i1 + c2.chg1;
        if e1 >= e2 {
            q += 1;
        }
        if e2 >= e1 {
            p += 1;
        }
    }
    for c1 in &x1[p..] {
        let i2 = (c1.i1 + ntheirs) as isize - nbase as isize;
        append(&mut recs, &mut bad, Mode::Ours, c1.i1, c1.chg1, c1.i2 as isize, c1.chg2, i2, c1.chg1);
    }
    for c2 in &x2[q..] {
        let i1 = (c2.i1 + nours) as isize - nbase as isize;
        append(&mut recs, &mut bad, Mode::Theirs, c2.i1, c2.chg1, i1, c2.chg1, c2.i2 as isize, c2.chg2);
    }
    // A region outside the files would mean the walk went wrong; the caller
    // then reports the whole file as one conflict (the safe side).
    let fits = |r: &Rec| r.i0 + r.chg0 <= nbase && r.i1 + r.chg1 <= nours && r.i2 + r.chg2 <= ntheirs;
    let mut prev_end = 0;
    for r in &recs {
        if !fits(r) || r.i1 < prev_end {
            bad = true;
        }
        prev_end = r.i1 + r.chg1;
    }
    if bad {
        return None;
    }
    for r in &mut recs {
        if r.mode == Mode::Conflict && ours[r.i1..r.i1 + r.chg1] == theirs[r.i2..r.i2 + r.chg2] {
            r.mode = Mode::Same;
        }
    }
    Some(recs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcs(a: &[u32], b: &[u32]) -> usize {
        let mut t = vec![vec![0usize; b.len() + 1]; a.len() + 1];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                t[i][j] = if a[i] == b[j] { t[i + 1][j + 1] + 1 } else { t[i + 1][j].max(t[i][j + 1]) };
            }
        }
        t[0][0]
    }

    /// The script is valid (applying it turns a into b) and minimal.
    #[test]
    fn diff_is_valid_and_minimal_on_random_inputs() {
        let mut seed: u64 = 0x9e3779b97f4a7c15;
        let mut rnd = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for _ in 0..3000 {
            let la = rnd(14) as usize;
            let lb = rnd(14) as usize;
            let alpha = 1 + rnd(5);
            let a: Vec<u32> = (0..la).map(|_| rnd(alpha) as u32).collect();
            let b: Vec<u32> = (0..lb).map(|_| rnd(alpha) as u32).collect();
            let script = diff(&a, &b);
            let mut out = Vec::new();
            let mut pos = 0;
            let mut cost = 0;
            for c in &script {
                assert!(c.i1 >= pos);
                out.extend_from_slice(&a[pos..c.i1]);
                out.extend_from_slice(&b[c.i2..c.i2 + c.chg2]);
                pos = c.i1 + c.chg1;
                cost += c.chg1 + c.chg2;
            }
            out.extend_from_slice(&a[pos..]);
            assert_eq!(out, b, "a={a:?} b={b:?} script={script:?}");
            assert_eq!(cost, la + lb - 2 * lcs(&a, &b), "a={a:?} b={b:?}");
        }
    }
}
