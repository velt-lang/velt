// The Node and Bun side of typed.vlt: JavaScript has no typed decode, so this is JSON.parse of
// the whole document followed by the same field reads and checksum as typed.vlt.
//   node typed.mjs <canada|citm|twitter> <file> <rounds>
import { readFileSync } from "node:fs";

const [corpus, file, roundsArg] = process.argv.slice(2);
const text = readFileSync(file, "utf8");
const rounds = Number(roundsArg);

function canadaSum(c) {
  let rings = 0;
  let points = 0;
  let s = 0;
  for (const f of c.features) {
    for (const ring of f.geometry.coordinates) {
      rings += 1;
      for (const p of ring) {
        points += 1;
        s += Math.round(p[0] * 100) + Math.round(p[1] * 100);
      }
    }
  }
  return `${c.features.length} ${rings} ${points} ${s}`;
}

function twitterSum(t) {
  let text = 0;
  let followers = 0;
  let tags = 0;
  let rt = 0;
  for (const st of t.statuses) {
    text += st.text.length + st.id_str.length + st.user.screen_name.length;
    followers += st.user.followers_count;
    tags += st.entities.hashtags.length;
    rt += st.retweet_count + st.favorite_count;
  }
  return `${t.statuses.length} ${text} ${followers} ${tags} ${rt} ${t.search_metadata.count}`;
}

function citmSum(c) {
  let prices = 0;
  let amount = 0;
  let areas = 0;
  for (const p of c.performances) {
    prices += p.prices.length;
    for (const pr of p.prices) amount += pr.amount;
    for (const sc of p.seatCategories) areas += sc.areas.length;
  }
  let topics = 0;
  const events = Object.values(c.events);
  for (const e of events) topics += e.topicIds.length + e.subTopicIds.length;
  return `${Object.keys(c.areaNames).length} ${events.length} ${c.performances.length} ${prices} ${amount} ${areas} ${topics}`;
}

const sum = { canada: canadaSum, twitter: twitterSum, citm: citmSum }[corpus];
let out = "";
for (let r = 0; r < rounds; r++) out = sum(JSON.parse(text));
console.log(out);
