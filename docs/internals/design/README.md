# Design notes

A design note explains one part of Velt's design: what it is, why it is that way, how it
compiles and what it costs. The notes are written so they can be published as the design section
of [veltlang.com](https://veltlang.com/docs/) next to the Book and the Reference: they describe
the design as it is now and the direction it is committed to, never the road that led there.

The Reference says what a program means; a design note says why, and how the compiler and the
runtime make it so. Where the two overlap, the Reference is the normative text and the note links
to it.

## Rules

### Content

1. **Only what is true.** Every statement about syntax, behavior, diagnostics, compiler passes,
   file paths and costs is true of `main`. A claim that cannot be checked against the code, the
   tests or a measurement does not belong in a note.
2. **The current design, not its history.** No "was", "used to", "before #N", "now" (as in
   "it now does"), "revised after", "round 2", "revision 6", no commit hashes, no dated owner or
   review decisions, no pull-request stacks and no step-by-step build logs. Git history and the
   issues keep that record. When the design changes, rewrite the note so it reads as if the new
   design had always been the plan.
3. **Decisions, stated as decisions.** A settled question is written as the rule it became, with
   its reason: "`r[k]` is `V | null`, because …", not "the open question was settled as
   recommended". Rejected alternatives go in "Alternatives considered", one paragraph each,
   saying why they lose, never who rejected them or when.
4. **Planned work is described as design, separated from what exists.** A part that is accepted
   but not built goes in a section headed "Not yet implemented" (or the status says the whole
   note is not built). It describes the intended behavior, not the order of pull requests.
   Work that is no longer planned is deleted.
5. **No superseded notes.** When a design is replaced, the replaced note is deleted in the same
   change and its links point at the new one. A note never says "replaced by" or "obsolete".
6. **Professional and public.** No people's names or handles, machine or session names, agent or
   crew references, internal channel names, or private repositories. Issue links
   (`[#123](https://github.com/velt-lang/velt/issues/123)`) are fine as references for readers
   who want the discussion; they are not a substitute for stating the decision in the note.
7. **Self-contained.** A reader who knows TypeScript can follow the note without the issue
   thread. Define Velt terms on first use or link the Reference page that does.

### Shape

Every note starts with a title and a status line:

```md
# <Feature name>

Status: implemented | partly implemented | accepted | proposed. Issue: [#N](https://github.com/velt-lang/velt/issues/N).
```

- **implemented**: everything in the note exists on `main`.
- **partly implemented**: the note has a "Not yet implemented" section; everything outside it exists.
- **accepted**: the design is agreed and not built yet.
- **proposed**: under review; may change or be dropped.

The status line holds nothing else: no dates, owners, steps or review rounds.

Then, in this order, the sections that apply (omit the ones that do not):

1. **Summary**: one or two paragraphs: what the feature is and what it buys.
2. **Motivation**: the problem, with real code that is awkward or slow without the feature.
3. **Design**: syntax (as close to TypeScript as possible), semantics, typing rules, interaction
   with the rest of the language. Examples are complete, valid Velt.
4. **Compilation**: how the front end, VIR and the backends implement it, naming the crates and
   modules (`crates/velt_sema/src/…`) a contributor would read.
5. **Runtime cost**: what it costs at run time and at compile time, with measurements (machine,
   benchmark, numbers) where they exist.
6. **Diagnostics**: the errors and warnings users see, with their wording and fixes.
7. **Compatibility**: how it relates to TypeScript and Node behavior; what valid TypeScript does.
8. **Alternatives considered**.
9. **Limitations**: what the design does not do, stated as facts.
10. **Not yet implemented**: the accepted parts that are not built (see rule 4).

### Style

- Present tense, active voice: "the compiler lowers `a + b` to …".
- Code paths are repository-relative and exist; examples use the `.velt` extension.
- Prefer a short table or list over a long paragraph for rules with cases.
- Lines wrap at 100 columns.
- No speculation about future versions beyond "Not yet implemented".

## Changing a design

A pull request that changes behavior described in a note updates the note in the same pull
request, rewriting the affected sections (rule 2) and the status line. A pull request that builds
a "Not yet implemented" part moves that text into the main sections. A new language feature
starts with a note in status **proposed** ([CONTRIBUTING.md](../../../CONTRIBUTING.md#proposing-a-language-change)).

The list of notes is in [docs/internals/README.md](../README.md#design-notes).
