//! Which intrinsics' arguments glue may format, serialise or deep-copy (`implicit.rs`, 1 and 2 of
//! its module docs): a match that lists every intrinsic, so a new one must be classified.

use crate::hir::Intrinsic;

/// The arguments of intrinsic `i` whose values glue may format, serialise or deep-copy, calling
/// class hooks (1 and 2 of the module docs): all of them, or none. The match lists every
/// intrinsic, so a new one must be classified.
pub(super) fn glue_args(i: Intrinsic) -> bool {
    use Intrinsic as I;
    match i {
        I::Print
        | I::PrintErr
        | I::ToString
        | I::ErrorText
        | I::Inspect
        | I::TemplateToString
        | I::Clone
        | I::Transfer
        | I::Spawn
        | I::SpawnHandled
        | I::SharedNew
        | I::SharedSet
        | I::MutexNew
        | I::MutexWith
        | I::RwLockNew
        | I::LockRead
        | I::PromiseAll
        | I::PromiseRace
        | I::PromiseAny
        // A member access, call or write through `any` may run any getter, method or setter of
        // the value held, and formats nothing itself; counted like a hook call on its arguments.
        | I::AnyGet
        | I::AnyIndex
        | I::AnyCall
        | I::AnyApply
        | I::AnySet
        | I::AnySetIndex
        // `key in p` runs a proxy's `has` trap (and a proxy's `get` / `set` slots its traps).
        | I::AnyHas
        // A record's `[RECORD_DELETE]` removes an entry.
        | I::AnyDelete
        // Reading an entry runs an object literal's getter; a hook is a method.
        | I::AnyReflectValue
        | I::AnyReflectHook
        // A heap future's poll and drop functions run any code of its body.
        | I::FutPoll
        | I::FutDrop => true,
        // Calls through a function's address; a slot's put and transfer transfer their value
        // (a store, of a value already transferred, runs no code), a take hands one out.
        I::StatePoll | I::StateDrop | I::AddressCall | I::CatchUnwindAt => true,
        I::AddressCallWord | I::AddressCall3 => true,
        I::SlotPut | I::SlotTake => true,
        I::SlotTransfer => true,
        I::FnAddress | I::SlotSize | I::SlotStore | I::PromiseAt | I::RawAlloc => false,
        I::SymbolAddress | I::SymbolAt => false,
        I::RawFree => false,
        // Shares the promise an `any` holds into a wrapper; formats and copies nothing.
        I::AnyPromise | I::AnyIsPromise => false,
        // Run no user code (what they drop is counted as drops; the function values they call
        // as callbacks; generator resumes apart).
        I::StrConcat
        | I::ThrowLoc
        | I::AnyTo
        | I::AnyToExact
        | I::AnyTypeof
        | I::AnyIsArray
        | I::AnyKeys
        | I::AnyTruthy
        | I::AnyTag
        | I::AnyUnboundMethod
        | I::Identity
        | I::ToAny
        | I::SpreadParam
        | I::AnyReflectInfo
        | I::AnyReflectCount
        | I::AnyReflectName
        | I::AnyToNumber
        | I::AnyStrictEq
        | I::AnyLooseEq
        | I::StrLen
        | I::StrByteLen
        | I::AddressLoad
        | I::AddressArray
        | I::AddressArrayOwned
        | I::AddressStore
        | I::AtomicAddress
        | I::AddressAtomicLoad
        | I::AddressAtomicStore
        | I::AddressAtomicRmw
        | I::AddressAtomicCas
        | I::AddressAtomicCasWeak
        | I::PtrFromAddress
        | I::PtrToAddress
        | I::PtrRead
        | I::PtrWrite
        | I::CArrived
        | I::ExportedAddress
        | I::Exit
        | I::Panic
        | I::ArrayWithCapacity
        | I::ArrayLen
        | I::ArrayPush
        | I::ArrayPop
        | I::ArraySwap
        | I::ArrayRemove
        | I::ArrayTruncate
        | I::ArrayMove
        | I::ArrayCopyBytes
        | I::ArrayDataAddress
        | I::ArrayReadAddress
        | I::ArrayBytesWritten
        | I::ArrayByteLength
        | I::ArraySetLen
        | I::ArrayReserve
        | I::ArrayPrefetch
        | I::AtomicLoad
        | I::AtomicStore
        | I::AtomicRmw
        | I::AtomicCas
        | I::AtomicCasWeak
        | I::AtomicFence
        | I::AtomicSpinHint
        | I::ThreadLocalGet
        | I::ThreadLocalSet
        | I::ArrayRotateRight
        | I::ArrayRotateLeft
        | I::ArraySwapNested
        | I::Subarray
        | I::ParamOrdering
        | I::Hash
        | I::KeyEq
        | I::IncludesEq
        | I::Same
        | I::SameValue
        | I::Share
        | I::FieldAbsent
        | I::FieldPresent
        | I::SpreadObject
        | I::KeyRead
        | I::KeyWrite
        | I::NeedsTransfer
        | I::NeedsDrop
        | I::ModuleStateUsed
        | I::MayAlias
        | I::IsUnique
        | I::WeakRead
        | I::HoldsNull
        | I::NullAs
        | I::Truthy
        | I::IsNull
        | I::IsNullish
        | I::LooseEq
        | I::MayHoldPrimitive
        | I::AnyMethodCalled
        | I::AnyIsList
        | I::NoteStdUse
        | I::StdUsed
        | I::HoldsDynamic
        | I::HoldsDynamicOrClass
        | I::FnCapturesNothing
        | I::FnLength
        | I::FnName
        | I::ClosureSibling(_)
        | I::InitSlots(..)
        | I::SimdShuffle(..)
        | I::Sqrt
        | I::Floor
        | I::Ceil
        | I::Round
        | I::Trunc
        | I::FAbs
        | I::Sleep
        | I::YieldNow
        | I::SourceLocation
        | I::PromiseWiden
        | I::PromiseSame
        | I::PerfNow
        | I::ProxyArrayCall
        | I::ProxyArrayHole
        | I::IsTypedProxy
        | I::DateNow
        | I::SharedAdd
        | I::SharedGet
        | I::SharedRead
        | I::StrCharCodeAt
        | I::StrByteAt
        | I::StrAsciiWords
        | I::StrbufPushAsciiWords
        | I::StrbufPushAsciiWord
        | I::StrLoadU8x16
        | I::StrLoadPartialU8x16
        | I::ArrayDataPtr
        | I::Attempt
        | I::CatchUnwind
        | I::Yield
        | I::GeneratorResume
        | I::GeneratorValue
        | I::GeneratorReturn
        | I::GeneratorResumeWith
        | I::GeneratorThrow
        | I::YieldDelegate
        | I::GeneratorEmbed
        | I::AsyncGeneratorResume
        | I::AsyncGeneratorValue
        | I::AsyncGeneratorReturn
        | I::AsyncGeneratorDispose
        | I::SymbolStatic
        | I::SymbolRegistry
        // Weak maps and refs (std/prelude/weak.velt): a value a map drops (a replaced or deleted
        // entry's, or a released key's) is counted as a drop (8 of the module docs).
        | I::WeakMapSet
        | I::WeakMapGet
        | I::WeakMapHas
        | I::WeakMapDelete
        | I::WeakMapValue
        | I::WeakRefNew
        | I::InspectNested
        | I::WeakRefDeref
        | I::WeakMapDrop
        | I::WeakRefDrop => false,
        // Record views: a view's source read or written through `any`, no formatting.
        I::RecordViewSync
        | I::RecordViewIs
        | I::RecordViewGet
        | I::RecordViewHas
        | I::RecordViewSet
        | I::RecordViewDelete
        | I::RecordViewFlush => false,
        I::StrWord0 | I::StrWord1 | I::StrWord2 | I::StrDataAddress | I::StrAt => false,
    }
}
