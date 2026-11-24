// The same decoder over `&[u8]`, as #1718 names the yardstick: `u16::from_be_bytes` and slices.
const PAGE: usize = 4096;
const PAGES: usize = 64;


fn put_varint(page: &mut [u8], at: usize, mut v: u64) -> usize {
    let mut bytes = vec![];
    loop {
        bytes.insert(0, (v & 0x7f) as u8);
        v /= 128;
        if v == 0 {
            break;
        }
    }
    let n = bytes.len();
    for (i, b) in bytes.into_iter().enumerate() {
        page[at + i] = if i < n - 1 { b | 0x80 } else { b };
    }
    n
}

fn build_page(seed: u64) -> Vec<u8> {
    let mut page = vec![0u8; PAGE];
    let mut content = PAGE;
    let mut pointers = vec![];
    let mut cells = 0u64;
    loop {
        let payload = 20 + ((seed * 7 + cells * 13) % 60) as usize;
        let cell_len = 2 + 3 + payload;
        if content < cell_len || content - cell_len < 8 + (pointers.len() + 1) * 2 + 16 {
            break;
        }
        content -= cell_len;
        let mut at = content;
        at += put_varint(&mut page, at, payload as u64);
        at += put_varint(&mut page, at, seed * 1000 + cells);
        for i in 0..payload {
            page[at + i] = ((seed + cells + i as u64) & 0xff) as u8;
        }
        pointers.push(content);
        cells += 1;
    }
    page[0] = 13;
    page[3..5].copy_from_slice(&(cells as u16).to_be_bytes());
    page[5..7].copy_from_slice(&(content as u16).to_be_bytes());
    for (i, p) in pointers.iter().enumerate() {
        page[8 + i * 2..10 + i * 2].copy_from_slice(&(*p as u16).to_be_bytes());
    }
    page
}

#[inline]
fn varint(page: &[u8], at: usize) -> (u64, usize) {
    let mut v = 0u64;
    let mut i = 0;
    while i < 8 {
        let b = page[at + i];
        v = v * 128 + (b & 0x7f) as u64;
        i += 1;
        if b < 0x80 {
            break;
        }
    }
    (v, i)
}

fn decode_page(page: &[u8]) -> u64 {
    if page[0] != 13 {
        return 0;
    }
    let cells = u16::from_be_bytes([page[3], page[4]]) as usize;
    let mut sum = 0u64;
    for i in 0..cells {
        let at = u16::from_be_bytes([page[8 + i * 2], page[9 + i * 2]]) as usize;
        let (payload, n1) = varint(page, at);
        let (rowid, n2) = varint(page, at + n1);
        let start = at + n1 + n2;
        let body = &page[start..start + payload as usize];
        let a = u32::from_be_bytes(body[0..4].try_into().unwrap()) as u64;
        let b = u32::from_le_bytes(body[4..8].try_into().unwrap()) as u64;
        sum += rowid + payload + a + b;
    }
    sum
}

fn main() {
    let pages: Vec<Vec<u8>> = (0..PAGES as u64).map(build_page).collect();
    let mut total = 0u64;
    let rounds: usize = std::env::args().nth(1).map_or(400, |s| s.parse().unwrap());
    for _ in 0..rounds {
        for p in &pages {
            total += decode_page(std::hint::black_box(p));
        }
    }
    println!("{total}");
}
