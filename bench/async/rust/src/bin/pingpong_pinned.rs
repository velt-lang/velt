// 4 owner tasks each keep a HashMap; 64 callers make 5000 request/reply calls each to the owner of
// their key, over an unbounded mpsc channel plus a oneshot for the reply (pingpong_pinned.vlt).
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};

struct Call {
    key: String,
    reply: oneshot::Sender<i64>,
}

fn main() {
    async_bench::run(async {
        let owners = 4;
        let mut inboxes = Vec::new();
        let mut running = Vec::new();
        for _ in 0..owners {
            let (tx, mut rx) = mpsc::unbounded_channel::<Call>();
            inboxes.push(tx);
            running.push(tokio::spawn(async move {
                let mut counts: HashMap<String, i64> = HashMap::new();
                while let Some(c) = rx.recv().await {
                    let n = counts.entry(c.key).or_insert(0);
                    *n += 1;
                    let _ = c.reply.send(*n);
                }
            }));
        }
        let mut callers = Vec::new();
        for c in 0..64usize {
            let inbox = inboxes[c % owners].clone();
            let key = format!("k{}", c % 16);
            callers.push(tokio::spawn(async move {
                let mut sum = 0i64;
                for _ in 0..5000 {
                    let (reply, answer) = oneshot::channel();
                    inbox
                        .send(Call {
                            key: key.clone(),
                            reply,
                        })
                        .expect("owner alive");
                    sum += answer.await.expect("owner replied");
                }
                sum
            }));
        }
        let mut total = 0i64;
        for c in callers {
            total += c.await.expect("caller");
        }
        drop(inboxes);
        for r in running {
            r.await.expect("owner");
        }
        println!("{total}");
    });
}
