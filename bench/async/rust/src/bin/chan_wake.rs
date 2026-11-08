// 64 tasks in a ring, each with its own unbounded mpsc channel, pass 8 tokens along (800k hops):
// every receive waits and is woken by the send of the task before it (chan_wake.velt).
use tokio::sync::mpsc;

const TASKS: usize = 64;
const TOKENS: usize = 8;
const HOPS: i64 = 100_000;

fn main() {
    async_bench::run(async {
        let mut senders = Vec::new();
        let mut receivers = Vec::new();
        for _ in 0..TASKS {
            let (tx, rx) = mpsc::unbounded_channel::<i64>();
            senders.push(tx);
            receivers.push(Some(rx));
        }
        let (done_tx, mut done_rx) = mpsc::unbounded_channel::<i64>();
        let mut members = Vec::new();
        for i in 0..TASKS {
            let mut inbox = receivers[i].take().expect("one receiver per task");
            let next = senders[(i + 1) % TASKS].clone();
            let done = done_tx.clone();
            members.push(tokio::spawn(async move {
                let mut seen = 0i64;
                while let Some(v) = inbox.recv().await {
                    if v < 0 {
                        break;
                    }
                    seen += 1;
                    if v + 1 >= HOPS {
                        let _ = done.send(seen);
                    } else {
                        let _ = next.send(v + 1);
                    }
                }
                seen
            }));
        }
        for t in 0..TOKENS {
            let _ = senders[t * (TASKS / TOKENS)].send(0);
        }
        for _ in 0..TOKENS {
            done_rx.recv().await;
        }
        // Every task holds the next task's sender, so dropping them closes nothing: -1 ends a task.
        for s in &senders {
            let _ = s.send(-1);
        }
        let mut hops = 0i64;
        for m in members {
            hops += m.await.expect("member");
        }
        println!("{hops}");
    });
}
