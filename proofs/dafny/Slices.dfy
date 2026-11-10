// Slices.dfy — swap, rotate, split and join (spec §3.13, §3.14).
//
// Names stop being buffers. A name now refers to a REGION of physical memory
// (a buffer, or a slice of one), and the statements that move storage between
// names — swap, rotate, split, join — change which region a name refers to.
// This is where phase-fuzz found real analyzer holes, because one physical
// cell can now be reachable under a name the checker is not thinking about.
//
// The run-time machine tracks ownership per memory CELL. `Start` hands every
// cell of a name's region to the engine; the CPU may touch a region only if
// it owns every cell in it; `join` is a pure pointer operation that faults
// physically if the two parts are not adjacent halves (a corrupt view), or if
// one of them is still in flight (a whole-buffer view over a half the engine
// is writing — #32).
//
// The checker does NOT know where any name points. It only knows, per name,
// its length, its domain, and which slice of which split buffer it holds,
// all of which travel with the storage on swap/rotate (§3.13). That is what
// makes ping-pong loops checkable: after `swap ping, pong` the names point at
// the other buffers, but the checker's view is the same as on entry.
//
// Theorem (Soundness): accepted programs never fault, on any path. Along the
// way the invariant proves the property that makes it all work: the regions
// of distinct live names never overlap, so handing one name's region to the
// engine can never take a cell from under another name.

module PhaseSlices {

  datatype Option<T> = None | Some(value: T)

  type Name = nat
  type BufId = nat

  // ---------------------------------------------------------------------
  // Syntax
  // ---------------------------------------------------------------------

  datatype Stmt =
    | Skip
    | Read(n: Name)
    | Write(n: Name)
    | Start(n: Name)                    // hand n's region to the engine
    | Sync(n: Name)                     // take it back
    | Split(n: Name, a: Name, b: Name)  // split n into a, b;
    | Join(a: Name, b: Name, n: Name)   // join a, b into n;
    | Swap(x: Name, y: Name)            // swap x, y;
    | Rotate(a: Name, b: Name, c: Name) // rotate a, b, c;   (a <- b <- c <- a)
    | Seq(s1: Stmt, s2: Stmt)
    | If(thn: Stmt, els: Stmt)
    | While(body: Stmt)

  function SwapNames<V>(m: map<Name, V>, x: Name, y: Name): map<Name, V>
    requires x in m && y in m
  {
    m[x := m[y]][y := m[x]]
  }

  function RotNames<V>(m: map<Name, V>, a: Name, b: Name, c: Name): map<Name, V>
    requires a in m && b in m && c in m
  {
    m[a := m[b]][b := m[c]][c := m[a]]
  }

  // ---------------------------------------------------------------------
  // Static side: what the analyzer knows
  // ---------------------------------------------------------------------

  datatype Dom = RAM | DMA

  // Which slice of which split buffer a name's storage is.
  datatype Slot = Slot(parent: Name, idx: nat)

  // Everything the checker knows about a name's storage. It travels with
  // the storage on swap and rotate.
  datatype Info = Info(len: nat, dom: Dom, slice: Option<Slot>)

  // A buffer that is currently split, and the slot its own storage holds
  // (so nested splits can be joined back up).
  datatype SplitRec = SplitRec(slice: Option<Slot>)

  datatype Env = Env(names: map<Name, Info>, splits: map<Name, SplitRec>)

  predicate RefersTo(s: Option<Slot>, n: Name) {
    s.Some? && s.value.parent == n
  }

  function SetDom(env: Env, n: Name, d: Dom): Env
    requires n in env.names
  {
    env.(names := env.names[n := env.names[n].(dom := d)])
  }

  // split: fixed length divisible by 2; while split, the parent is not a
  // usable name at all (#31)
  predicate SplitGuard(env: Env, n: Name, a: Name, b: Name) {
    n in env.names && a != b && a !in env.names && b !in env.names
    && a !in env.splits && b !in env.splits
    && env.names[n].len >= 2 && env.names[n].len % 2 == 0
  }

  function SplitEnv(env: Env, n: Name, a: Name, b: Name): Env
    requires n in env.names
  {
    var i := env.names[n];
    Env((env.names - {n})[a := Info(i.len / 2, i.dom, Some(Slot(n, 0)))]
                         [b := Info(i.len / 2, i.dom, Some(Slot(n, 1)))],
        env.splits[n := SplitRec(i.slice)])
  }

  // join: names exactly the parts, in order, each still holding its own
  // slice, all in one domain (#32), which the buffer then has
  predicate JoinGuard(env: Env, a: Name, b: Name, n: Name) {
    a != b && a in env.names && b in env.names && n in env.splits
    && env.names[a].slice == Some(Slot(n, 0))
    && env.names[b].slice == Some(Slot(n, 1))
    && env.names[a].dom == env.names[b].dom
    && env.names[a].len == env.names[b].len
    // Implied by the invariant (each slot is held by exactly one storage);
    // checked here to keep the proof short.
    && (forall m :: m in env.names && m != a && m != b ==> !RefersTo(env.names[m].slice, n))
    && (forall q :: q in env.splits ==> !RefersTo(env.splits[q].slice, n))
  }

  function JoinEnv(env: Env, a: Name, b: Name, n: Name): Env
    requires a in env.names && n in env.splits
  {
    Env((env.names - {a, b})[n := Info(2 * env.names[a].len, env.names[a].dom,
                                       env.splits[n].slice)],
        env.splits - {n})
  }

  // swap/rotate: same buffer type; everything known moves with the storage
  predicate SwapGuard(env: Env, x: Name, y: Name) {
    x != y && x in env.names && y in env.names && env.names[x].len == env.names[y].len
  }

  predicate RotGuard(env: Env, a: Name, b: Name, c: Name) {
    a != b && b != c && a != c
    && a in env.names && b in env.names && c in env.names
    && env.names[a].len == env.names[b].len && env.names[b].len == env.names[c].len
  }

  function Check(env: Env, s: Stmt): Option<Env>
    decreases s
  {
    match s
    case Skip => Some(env)
    case Read(n) =>
      if n in env.names && env.names[n].dom == RAM then Some(env) else None
    case Write(n) =>
      if n in env.names && env.names[n].dom == RAM then Some(env) else None
    case Start(n) =>
      if n in env.names && env.names[n].dom == RAM then Some(SetDom(env, n, DMA)) else None
    case Sync(n) =>
      if n in env.names && env.names[n].dom == DMA then Some(SetDom(env, n, RAM)) else None
    case Split(n, a, b) =>
      if SplitGuard(env, n, a, b) then Some(SplitEnv(env, n, a, b)) else None
    case Join(a, b, n) =>
      if JoinGuard(env, a, b, n) then Some(JoinEnv(env, a, b, n)) else None
    case Swap(x, y) =>
      if SwapGuard(env, x, y) then Some(env.(names := SwapNames(env.names, x, y))) else None
    case Rotate(a, b, c) =>
      if RotGuard(env, a, b, c) then Some(env.(names := RotNames(env.names, a, b, c))) else None
    case Seq(s1, s2) =>
      (match Check(env, s1)
       case None => None
       case Some(mid) => Check(mid, s2))
    case If(t, e) =>
      // split, or not, on every path into a merge
      (match (Check(env, t), Check(env, e))
       case (Some(x), Some(y)) => if x == y then Some(x) else None
       case _ => None)
    case While(body) =>
      // a body must leave every name as it found it (and join what it split)
      if Check(env, body) == Some(env) then Some(env) else None
  }

  // ---------------------------------------------------------------------
  // Dynamic side: physical memory, cell by cell
  // ---------------------------------------------------------------------

  datatype Owner = Cpu | Engine
  datatype Cell = Cell(buf: BufId, i: int)
  datatype Region = Region(buf: BufId, lo: int, hi: int)   // [lo, hi)

  // names: where each live name points.  vreg: for each split buffer, the
  // region it covered when split (bookkeeping only: no step reads it).
  datatype State = State(names: map<Name, Region>, owner: map<Cell, Owner>,
                         vreg: map<Name, Region>)

  datatype Outcome = Ok(st: State) | Fault

  function ToOwner(d: Dom): Owner {
    if d == RAM then Cpu else Engine
  }

  predicate In(c: Cell, r: Region) {
    c.buf == r.buf && r.lo <= c.i < r.hi
  }

  predicate AllOwned(owner: map<Cell, Owner>, r: Region, o: Owner) {
    forall i :: r.lo <= i < r.hi ==> Cell(r.buf, i) in owner && owner[Cell(r.buf, i)] == o
  }

  function SetOwner(owner: map<Cell, Owner>, r: Region, o: Owner): map<Cell, Owner> {
    map c | c in owner.Keys :: if In(c, r) then o else owner[c]
  }

  function Half(r: Region, i: nat): Region {
    var mid := r.lo + (r.hi - r.lo) / 2;
    if i == 0 then Region(r.buf, r.lo, mid) else Region(r.buf, mid, r.hi)
  }

  // The CPU may touch / hand over / take back n only if the right side owns
  // every cell of it.
  predicate OwnedBy(st: State, n: Name, d: Dom) {
    n in st.names && AllOwned(st.owner, st.names[n], ToOwner(d))
  }

  function MoveSt(st: State, n: Name, d: Dom): State
    requires n in st.names
  {
    st.(owner := SetOwner(st.owner, st.names[n], ToOwner(d)))
  }

  predicate SplitRT(st: State, n: Name, a: Name, b: Name) {
    n in st.names && a !in st.names && b !in st.names && a != b
    && st.names[n].hi - st.names[n].lo >= 2
    && (st.names[n].hi - st.names[n].lo) % 2 == 0
  }

  function SplitSt(st: State, n: Name, a: Name, b: Name): State
    requires n in st.names
  {
    var r := st.names[n];
    st.(names := (st.names - {n})[a := Half(r, 0)][b := Half(r, 1)],
        vreg := st.vreg[n := r])
  }

  // `join` emits nothing: it just makes one pointer of two. It is only
  // physically meaningful for adjacent parts both owned by one side.
  predicate JoinRT(st: State, a: Name, b: Name, n: Name) {
    a in st.names && b in st.names && a != b && n !in st.names
    && st.names[a].buf == st.names[b].buf
    && st.names[a].hi == st.names[b].lo
    && ((AllOwned(st.owner, st.names[a], Cpu) && AllOwned(st.owner, st.names[b], Cpu))
        || (AllOwned(st.owner, st.names[a], Engine) && AllOwned(st.owner, st.names[b], Engine)))
  }

  function JoinSt(st: State, a: Name, b: Name, n: Name): State
    requires a in st.names && b in st.names
  {
    st.(names := (st.names - {a, b})[n := Region(st.names[a].buf, st.names[a].lo,
                                                 st.names[b].hi)],
        vreg := st.vreg - {n})
  }

  least predicate Exec(s: Stmt, st: State, out: Outcome)
  {
    match s
    case Skip => out == Ok(st)
    case Read(n) =>
      out == (if OwnedBy(st, n, RAM) then Ok(st) else Fault)
    case Write(n) =>
      out == (if OwnedBy(st, n, RAM) then Ok(st) else Fault)
    case Start(n) =>
      out == (if OwnedBy(st, n, RAM) then Ok(MoveSt(st, n, DMA)) else Fault)
    case Sync(n) =>
      out == (if OwnedBy(st, n, DMA) then Ok(MoveSt(st, n, RAM)) else Fault)
    case Split(n, a, b) =>
      out == (if SplitRT(st, n, a, b) then Ok(SplitSt(st, n, a, b)) else Fault)
    case Join(a, b, n) =>
      out == (if JoinRT(st, a, b, n) then Ok(JoinSt(st, a, b, n)) else Fault)
    case Swap(x, y) =>
      out == (if x in st.names && y in st.names && x != y
              then Ok(st.(names := SwapNames(st.names, x, y))) else Fault)
    case Rotate(a, b, c) =>
      out == (if a in st.names && b in st.names && c in st.names
                 && a != b && b != c && a != c
              then Ok(st.(names := RotNames(st.names, a, b, c))) else Fault)
    case Seq(s1, s2) =>
      (!out.Ok? && Exec(s1, st, out))
      || (exists mid :: Exec(s1, st, Ok(mid)) && Exec(s2, mid, out))
    case If(t, e) =>
      Exec(t, st, out) || Exec(e, st, out)
    case While(body) =>
      out == Ok(st)
      || (!out.Ok? && Exec(body, st, out))
      || (exists mid :: Exec(body, st, Ok(mid)) && Exec(While(body), mid, out))
  }

  // ---------------------------------------------------------------------
  // The invariant, in three opaque parts so each proof unfolds only what
  // it needs
  // ---------------------------------------------------------------------

  predicate WF(r: Region) { r.lo < r.hi }

  predicate Disjoint(r1: Region, r2: Region) {
    r1.buf != r2.buf || r1.hi <= r2.lo || r2.hi <= r1.lo
  }

  ghost predicate Shape(env: Env, st: State) {
    env.names.Keys == st.names.Keys
    && env.splits.Keys == st.vreg.Keys
    && (forall n :: n in env.names ==> n !in env.splits)
  }

  // every live name points at a well-formed region of the right length,
  // all of whose cells are owned by the side its domain says
  opaque ghost predicate Owned(env: Env, names: map<Name, Region>, owner: map<Cell, Owner>) {
    forall n :: n in names && n in env.names ==>
      WF(names[n])
      && names[n].hi - names[n].lo == env.names[n].len
      && AllOwned(owner, names[n], ToOwner(env.names[n].dom))
  }

  // no cell is reachable under two live names
  opaque ghost predicate NoOverlap(names: map<Name, Region>) {
    forall n, m :: n in names && m in names && n != m ==> Disjoint(names[n], names[m])
  }

  // what the checker thinks a slice is, it is
  opaque ghost predicate Slices(env: Env, names: map<Name, Region>, vreg: map<Name, Region>) {
    (forall n :: n in env.names && n in names && env.names[n].slice.Some? ==>
       env.names[n].slice.value.parent in vreg
       && names[n] == Half(vreg[env.names[n].slice.value.parent], env.names[n].slice.value.idx))
    && (forall q :: q in env.splits && q in vreg && env.splits[q].slice.Some? ==>
          env.splits[q].slice.value.parent in vreg
          && vreg[q] == Half(vreg[env.splits[q].slice.value.parent], env.splits[q].slice.value.idx))
    && (forall p :: p in vreg ==> WF(vreg[p]) && (vreg[p].hi - vreg[p].lo) % 2 == 0)
  }

  ghost predicate Inv(env: Env, st: State) {
    Shape(env, st)
    && Owned(env, st.names, st.owner)
    && NoOverlap(st.names)
    && Slices(env, st.names, st.vreg)
  }

  lemma OwnedAt(env: Env, names: map<Name, Region>, owner: map<Cell, Owner>, n: Name)
    requires Owned(env, names, owner) && n in names && n in env.names
    ensures WF(names[n]) && names[n].hi - names[n].lo == env.names[n].len
    ensures AllOwned(owner, names[n], ToOwner(env.names[n].dom))
  {
    reveal Owned();
  }

  lemma DisjointAt(names: map<Name, Region>, n: Name, m: Name)
    requires NoOverlap(names) && n in names && m in names && n != m
    ensures Disjoint(names[n], names[m])
  {
    reveal NoOverlap();
  }

  lemma SliceAt(env: Env, names: map<Name, Region>, vreg: map<Name, Region>, n: Name)
    requires Slices(env, names, vreg)
    requires n in env.names && n in names && env.names[n].slice.Some?
    ensures env.names[n].slice.value.parent in vreg
    ensures names[n] == Half(vreg[env.names[n].slice.value.parent], env.names[n].slice.value.idx)
  {
    reveal Slices();
  }

  lemma SplitSliceAt(env: Env, names: map<Name, Region>, vreg: map<Name, Region>, q: Name)
    requires Slices(env, names, vreg)
    requires q in env.splits && q in vreg && env.splits[q].slice.Some?
    ensures env.splits[q].slice.value.parent in vreg
    ensures vreg[q] == Half(vreg[env.splits[q].slice.value.parent], env.splits[q].slice.value.idx)
  {
    reveal Slices();
  }

  lemma VregAt(env: Env, names: map<Name, Region>, vreg: map<Name, Region>, p: Name)
    requires Slices(env, names, vreg) && p in vreg
    ensures WF(vreg[p]) && (vreg[p].hi - vreg[p].lo) % 2 == 0
  {
    reveal Slices();
  }

  // ---------------------------------------------------------------------
  // Lemmas about cells and regions
  // ---------------------------------------------------------------------

  lemma SetOwnerSelf(owner: map<Cell, Owner>, r: Region, o: Owner, o0: Owner)
    requires AllOwned(owner, r, o0)
    ensures AllOwned(SetOwner(owner, r, o), r, o)
  {
    forall i | r.lo <= i < r.hi
      ensures Cell(r.buf, i) in SetOwner(owner, r, o) && SetOwner(owner, r, o)[Cell(r.buf, i)] == o
    {
      assert In(Cell(r.buf, i), r);
    }
  }

  lemma SetOwnerOther(owner: map<Cell, Owner>, r: Region, o: Owner, m: Region, om: Owner)
    requires AllOwned(owner, m, om)
    requires Disjoint(m, r)
    ensures AllOwned(SetOwner(owner, r, o), m, om)
  {
    forall i | m.lo <= i < m.hi
      ensures Cell(m.buf, i) in SetOwner(owner, r, o) && SetOwner(owner, r, o)[Cell(m.buf, i)] == om
    {
      assert !In(Cell(m.buf, i), r);
    }
  }

  lemma AllOwnedSub(owner: map<Cell, Owner>, r: Region, sub: Region, o: Owner)
    requires AllOwned(owner, r, o)
    requires sub.buf == r.buf && r.lo <= sub.lo && sub.hi <= r.hi
    ensures AllOwned(owner, sub, o)
  {}

  lemma AllOwnedUnion(owner: map<Cell, Owner>, h0: Region, h1: Region, o: Owner)
    requires AllOwned(owner, h0, o) && AllOwned(owner, h1, o)
    requires h0.buf == h1.buf && h0.hi == h1.lo
    ensures AllOwned(owner, Region(h0.buf, h0.lo, h1.hi), o)
  {
    forall i | h0.lo <= i < h1.hi
      ensures Cell(h0.buf, i) in owner && owner[Cell(h0.buf, i)] == o
    {
      if i < h0.hi { assert h0.lo <= i < h0.hi; } else { assert h1.lo <= i < h1.hi; }
    }
  }

  lemma HalvesOf(r: Region)
    requires WF(r) && (r.hi - r.lo) % 2 == 0
    ensures Half(r, 0).buf == r.buf && Half(r, 1).buf == r.buf
    ensures Half(r, 0).lo == r.lo && Half(r, 1).hi == r.hi
    ensures Half(r, 0).hi == Half(r, 1).lo
    ensures WF(Half(r, 0)) && WF(Half(r, 1))
    ensures Half(r, 0).hi - Half(r, 0).lo == (r.hi - r.lo) / 2
    ensures Half(r, 1).hi - Half(r, 1).lo == (r.hi - r.lo) / 2
    ensures Disjoint(Half(r, 0), Half(r, 1)) && Disjoint(Half(r, 1), Half(r, 0))
  {}

  // ---------------------------------------------------------------------
  // One lemma per statement
  // ---------------------------------------------------------------------

  lemma ReadOk(env: Env, st: State, n: Name)
    requires Inv(env, st) && n in env.names && env.names[n].dom == RAM
    ensures OwnedBy(st, n, RAM)
  {
    OwnedAt(env, st.names, st.owner, n);
  }

  // Start and Sync
  lemma MoveOk(env: Env, st: State, n: Name, from: Dom, to: Dom)
    requires Inv(env, st) && n in env.names && env.names[n].dom == from
    ensures OwnedBy(st, n, from)
    ensures Inv(SetDom(env, n, to), MoveSt(st, n, to))
  {
    var r := st.names[n];
    var env' := SetDom(env, n, to);
    var st' := MoveSt(st, n, to);
    OwnedAt(env, st.names, st.owner, n);
    SetOwnerSelf(st.owner, r, ToOwner(to), ToOwner(from));
    forall m | m in st.names && m in env.names && m != n
      ensures AllOwned(st'.owner, st.names[m], ToOwner(env.names[m].dom))
    {
      OwnedAt(env, st.names, st.owner, m);
      DisjointAt(st.names, m, n);
      SetOwnerOther(st.owner, r, ToOwner(to), st.names[m], ToOwner(env.names[m].dom));
    }
    assert Shape(env', st');
    assert Owned(env', st'.names, st'.owner) by {
      reveal Owned();
    }
    assert Slices(env', st'.names, st'.vreg) by {
      reveal Slices();
    }
  }

  lemma SplitOk(env: Env, st: State, n: Name, a: Name, b: Name)
    requires Inv(env, st) && SplitGuard(env, n, a, b)
    ensures SplitRT(st, n, a, b)
    ensures Inv(SplitEnv(env, n, a, b), SplitSt(st, n, a, b))
  {
    var i := env.names[n];
    var r := st.names[n];
    OwnedAt(env, st.names, st.owner, n);
    HalvesOf(r);
    var h0 := Half(r, 0);
    var h1 := Half(r, 1);
    var env' := SplitEnv(env, n, a, b);
    var st' := SplitSt(st, n, a, b);
    assert st'.names == (st.names - {n})[a := h0][b := h1];
    assert st'.vreg == st.vreg[n := r];
    assert n !in st.vreg;
    var o := ToOwner(i.dom);
    AllOwnedSub(st.owner, r, h0, o);
    AllOwnedSub(st.owner, r, h1, o);
    assert Shape(env', st');
    assert Owned(env', st'.names, st'.owner) by {
      reveal Owned();
    }
    assert NoOverlap(st'.names) by {
      reveal NoOverlap();
      forall p, q | p in st'.names && q in st'.names && p != q
        ensures Disjoint(st'.names[p], st'.names[q])
      {
        if p != a && p != b && q != a && q != b {
          assert st'.names[p] == st.names[p] && st'.names[q] == st.names[q];
        } else if p != a && p != b {
          assert p in st.names && p != n;
          assert Disjoint(st.names[p], r);
        } else if q != a && q != b {
          assert q in st.names && q != n;
          assert Disjoint(r, st.names[q]);
        }
      }
    }
    assert Slices(env', st'.names, st'.vreg) by {
      reveal Slices();
      if i.slice.Some? {
        assert i.slice.value.parent in st.vreg;
        assert r == Half(st.vreg[i.slice.value.parent], i.slice.value.idx);
      }
    }
  }

  lemma JoinOk(env: Env, st: State, a: Name, b: Name, n: Name)
    requires Inv(env, st) && JoinGuard(env, a, b, n)
    ensures JoinRT(st, a, b, n)
    ensures Inv(JoinEnv(env, a, b, n), JoinSt(st, a, b, n))
  {
    assert n in st.vreg && n !in st.names;
    var R := st.vreg[n];
    SliceAt(env, st.names, st.vreg, a);
    SliceAt(env, st.names, st.vreg, b);
    assert st.names[a] == Half(R, 0) && st.names[b] == Half(R, 1);
    VregAt(env, st.names, st.vreg, n);
    HalvesOf(R);
    OwnedAt(env, st.names, st.owner, a);
    OwnedAt(env, st.names, st.owner, b);
    var o := ToOwner(env.names[a].dom);
    assert o == Cpu || o == Engine;
    AllOwnedUnion(st.owner, st.names[a], st.names[b], o);
    var env' := JoinEnv(env, a, b, n);
    var st' := JoinSt(st, a, b, n);
    assert st'.names == (st.names - {a, b})[n := R];
    assert st'.vreg == st.vreg - {n};
    var rec := env.splits[n];
    if rec.slice.Some? {
      SplitSliceAt(env, st.names, st.vreg, n);
      assert rec.slice.value.parent != n;
    }
    forall m | m in st.names && m != a && m != b
      ensures Disjoint(st.names[m], R) && Disjoint(R, st.names[m])
    {
      OwnedAt(env, st.names, st.owner, m);
      DisjointAt(st.names, m, a);
      DisjointAt(st.names, m, b);
    }
    assert Shape(env', st');
    assert Owned(env', st'.names, st'.owner) by {
      reveal Owned();
    }
    assert NoOverlap(st'.names) by {
      reveal NoOverlap();
    }
    assert Slices(env', st'.names, st'.vreg) by {
      reveal Slices();
    }
  }

  // swap and rotate permute where names point, and what the checker knows
  // about each, by the same permutation
  lemma PermOwned(env: Env, names: map<Name, Region>, owner: map<Cell, Owner>,
                  env': Env, names': map<Name, Region>, pi: Name -> Name)
    requires Owned(env, names, owner)
    requires names'.Keys == names.Keys && env'.names.Keys == env.names.Keys
    requires env.names.Keys == names.Keys
    requires forall n :: n in names ==> pi(n) in names && pi(n) in env.names
    requires forall n :: n in names ==> names'[n] == names[pi(n)]
    requires forall n :: n in env.names ==> env'.names[n] == env.names[pi(n)]
    ensures Owned(env', names', owner)
  {
    reveal Owned();
    forall n | n in names' && n in env'.names
      ensures WF(names'[n]) && names'[n].hi - names'[n].lo == env'.names[n].len
           && AllOwned(owner, names'[n], ToOwner(env'.names[n].dom))
    {
      var k := pi(n);
      assert names'[n] == names[k] && env'.names[n] == env.names[k];
    }
  }

  lemma PermNoOverlap(names: map<Name, Region>, names': map<Name, Region>, pi: Name -> Name)
    requires NoOverlap(names)
    requires names'.Keys == names.Keys
    requires forall n :: n in names ==> pi(n) in names && names'[n] == names[pi(n)]
    requires forall n, m :: n in names && m in names && n != m ==> pi(n) != pi(m)
    ensures NoOverlap(names')
  {
    reveal NoOverlap();
    forall n, m | n in names' && m in names' && n != m
      ensures Disjoint(names'[n], names'[m])
    {
      assert pi(n) != pi(m);
      assert names'[n] == names[pi(n)] && names'[m] == names[pi(m)];
    }
  }

  lemma PermSlices(env: Env, names: map<Name, Region>, vreg: map<Name, Region>,
                   env': Env, names': map<Name, Region>, pi: Name -> Name)
    requires Slices(env, names, vreg)
    requires env'.splits == env.splits
    requires names'.Keys == names.Keys && env'.names.Keys == env.names.Keys
    requires env.names.Keys == names.Keys
    requires forall n :: n in names ==> pi(n) in names && names'[n] == names[pi(n)]
    requires forall n :: n in env.names ==> env'.names[n] == env.names[pi(n)]
    ensures Slices(env', names', vreg)
  {
    reveal Slices();
    forall n | n in env'.names && n in names' && env'.names[n].slice.Some?
      ensures env'.names[n].slice.value.parent in vreg
           && names'[n] == Half(vreg[env'.names[n].slice.value.parent],
                                env'.names[n].slice.value.idx)
    {
      var k := pi(n);
      assert names'[n] == names[k] && env'.names[n] == env.names[k];
    }
  }

  lemma SwapOk(env: Env, st: State, x: Name, y: Name)
    requires Inv(env, st) && SwapGuard(env, x, y)
    ensures x in st.names && y in st.names && x != y
    ensures Inv(env.(names := SwapNames(env.names, x, y)),
                st.(names := SwapNames(st.names, x, y)))
  {
    var pi := (k: Name) => if k == x then y else if k == y then x else k;
    var env' := env.(names := SwapNames(env.names, x, y));
    var st' := st.(names := SwapNames(st.names, x, y));
    assert forall n :: n in st.names ==> st'.names[n] == st.names[pi(n)];
    assert forall n :: n in env.names ==> env'.names[n] == env.names[pi(n)];
    PermOwned(env, st.names, st.owner, env', st'.names, pi);
    PermNoOverlap(st.names, st'.names, pi);
    PermSlices(env, st.names, st.vreg, env', st'.names, pi);
    assert Shape(env', st');
  }

  lemma RotOk(env: Env, st: State, a: Name, b: Name, c: Name)
    requires Inv(env, st) && RotGuard(env, a, b, c)
    ensures a in st.names && b in st.names && c in st.names && a != b && b != c && a != c
    ensures Inv(env.(names := RotNames(env.names, a, b, c)),
                st.(names := RotNames(st.names, a, b, c)))
  {
    var pi := (k: Name) => if k == a then b else if k == b then c else if k == c then a else k;
    var env' := env.(names := RotNames(env.names, a, b, c));
    var st' := st.(names := RotNames(st.names, a, b, c));
    assert forall n :: n in st.names ==> st'.names[n] == st.names[pi(n)];
    assert forall n :: n in env.names ==> env'.names[n] == env.names[pi(n)];
    PermOwned(env, st.names, st.owner, env', st'.names, pi);
    PermNoOverlap(st.names, st'.names, pi);
    PermSlices(env, st.names, st.vreg, env', st'.names, pi);
    assert Shape(env', st');
  }

  // ---------------------------------------------------------------------
  // Soundness
  // ---------------------------------------------------------------------

  least lemma Soundness(s: Stmt, st: State, out: Outcome, env: Env, env': Env)
    requires Exec(s, st, out)
    requires Check(env, s) == Some(env')
    requires Inv(env, st)
    ensures out.Ok? && Inv(env', out.st)
  {
    match s
    case Skip =>
    case Read(n) => ReadOk(env, st, n);
    case Write(n) => ReadOk(env, st, n);
    case Start(n) => MoveOk(env, st, n, RAM, DMA);
    case Sync(n) => MoveOk(env, st, n, DMA, RAM);
    case Split(n, a, b) => SplitOk(env, st, n, a, b);
    case Join(a, b, n) => JoinOk(env, st, a, b, n);
    case Swap(x, y) => SwapOk(env, st, x, y);
    case Rotate(a, b, c) => RotOk(env, st, a, b, c);
    case Seq(s1, s2) =>
      var mid_env := Check(env, s1).value;
      if !out.Ok? && Exec(s1, st, out) {
        Soundness(s1, st, out, env, mid_env);   // contradiction
      } else {
        var mid :| Exec(s1, st, Ok(mid)) && Exec(s2, mid, out);
        Soundness(s1, st, Ok(mid), env, mid_env);
        Soundness(s2, mid, out, mid_env, env');
      }
    case If(t, e) =>
      assert Check(env, t) == Some(env');
      assert Check(env, e) == Some(env');
      if Exec(t, st, out) {
        Soundness(t, st, out, env, env');
      } else {
        Soundness(e, st, out, env, env');
      }
    case While(body) =>
      assert env' == env;
      assert Check(env, body) == Some(env);
      if out == Ok(st) {
      } else if !out.Ok? && Exec(body, st, out) {
        Soundness(body, st, out, env, env);     // contradiction
      } else {
        var mid :| Exec(body, st, Ok(mid)) && Exec(While(body), mid, out);
        Soundness(body, st, Ok(mid), env, env);
        Soundness(While(body), mid, out, env, env);
      }
  }

  // ---------------------------------------------------------------------
  // The bug-gallery shapes, and the patterns the language exists for
  // ---------------------------------------------------------------------

  // One 4-element buffer `0`, CPU-owned.
  function E1(): Env { Env(map[0 := Info(4, RAM, None)], map[]) }

  // `split 0 into 2, 3;` as the checker sees it
  function E1Split(): Env { SplitEnv(E1(), 0, 2, 3) }

  lemma E1SplitFacts()
    ensures E1Split().names.Keys == {2, 3}
    ensures E1Split().names[2] == Info(2, RAM, Some(Slot(0, 0)))
    ensures E1Split().names[3] == Info(2, RAM, Some(Slot(0, 1)))
    ensures E1Split().splits == map[0 := SplitRec(None)]
    ensures Check(E1(), Split(0, 2, 3)) == Some(E1Split())
  {}

  // use_ring_while_split.phase (#31)
  lemma UseWhileSplitIsRejected()
    ensures Check(E1(), Seq(Split(0, 2, 3), Read(0))) == None
  {
    E1SplitFacts();
  }

  // join_with_half_in_flight.phase (#32)
  lemma JoinWithHalfInFlightIsRejected()
    ensures Check(E1(), Seq(Split(0, 2, 3), Seq(Start(2), Join(2, 3, 0)))) == None
  {
    E1SplitFacts();
    var e := E1Split();
    var e2 := SetDom(e, 2, DMA);
    assert Check(e, Start(2)) == Some(e2);
    assert e2.names[2].dom == DMA && e2.names[3].dom == RAM;
    assert !JoinGuard(e2, 2, 3, 0);
  }

  // "a part that was swapped away has to be swapped back first"
  function SwappedJoin(): Stmt { Seq(Split(0, 2, 3), Seq(Swap(2, 3), Join(2, 3, 0))) }

  lemma SwappedPartsJoinIsRejected()
    ensures Check(E1(), SwappedJoin()) == None
  {
    E1SplitFacts();
    var e := E1Split();
    var e2 := e.(names := SwapNames(e.names, 2, 3));
    assert Check(e, Swap(2, 3)) == Some(e2);
    assert e2.names[2].slice == Some(Slot(0, 1));
    assert !JoinGuard(e2, 2, 3, 0);
  }

  // ...and without the checker it really is a corrupt view: after the swap
  // the "first" part is the upper half, so the two are not adjacent.
  function S1(): State {
    State(map[0 := Region(0, 0, 4)],
          map[Cell(0, 0) := Cpu, Cell(0, 1) := Cpu, Cell(0, 2) := Cpu, Cell(0, 3) := Cpu],
          map[])
  }

  lemma SwappedPartsJoinFaults()
    ensures Exec(SwappedJoin(), S1(), Fault)
  {
    var s0 := S1();
    var r := s0.names[0];
    assert Half(r, 0) == Region(0, 0, 2) && Half(r, 1) == Region(0, 2, 4);
    assert SplitRT(s0, 0, 2, 3);
    var s1 := SplitSt(s0, 0, 2, 3);
    assert s1.names == map[2 := Region(0, 0, 2), 3 := Region(0, 2, 4)];
    assert Exec(Split(0, 2, 3), s0, Ok(s1));
    var s2 := s1.(names := SwapNames(s1.names, 2, 3));
    assert Exec(Swap(2, 3), s1, Ok(s2));
    assert s2.names[2] == Region(0, 2, 4) && s2.names[3] == Region(0, 0, 2);
    assert !JoinRT(s2, 2, 3, 0);
    assert Exec(Join(2, 3, 0), s2, Fault);
    assert Exec(Seq(Swap(2, 3), Join(2, 3, 0)), s1, Fault);
  }

  // circular_dma.phase, in miniature: split, hand both halves to the
  // engine, take each back and process it, join.
  lemma CircularDmaIsAccepted()
    ensures Check(E1(), Seq(Split(0, 2, 3),
                        Seq(Start(2), Seq(Start(3),
                        Seq(Sync(2), Seq(Read(2), Seq(Sync(3), Join(2, 3, 0))))))))
            .Some?
  {
    E1SplitFacts();
    var e := E1Split();
    var e2 := SetDom(e, 2, DMA);
    var e3 := SetDom(e2, 3, DMA);
    var e4 := SetDom(e3, 2, RAM);
    var e5 := SetDom(e4, 3, RAM);
    assert Check(e, Start(2)) == Some(e2);
    assert Check(e2, Start(3)) == Some(e3);
    assert Check(e3, Sync(2)) == Some(e4);
    assert Check(e4, Read(2)) == Some(e4);
    assert Check(e4, Sync(3)) == Some(e5);
    assert e5.names.Keys == {2, 3};
    assert e5.names[2] == Info(2, RAM, Some(Slot(0, 0)));
    assert e5.names[3] == Info(2, RAM, Some(Slot(0, 1)));
    assert e5.splits == map[0 := SplitRec(None)];
    assert JoinGuard(e5, 2, 3, 0);
    assert Check(e5, Join(2, 3, 0)).Some?;
    assert Check(e4, Seq(Sync(3), Join(2, 3, 0))).Some?;
    assert Check(e4, Seq(Read(2), Seq(Sync(3), Join(2, 3, 0)))).Some?;
    assert Check(e3, Seq(Sync(2), Seq(Read(2), Seq(Sync(3), Join(2, 3, 0))))).Some?;
    assert Check(e2, Seq(Start(3), Seq(Sync(2), Seq(Read(2), Seq(Sync(3), Join(2, 3, 0)))))).Some?;
  }

  // ping_pong.phase: `ping` (0) in flight on entry, `pong` (1) free. Each
  // iteration takes ping back, starts pong, processes ping, and swaps --
  // which leaves the checker's view exactly as on entry.
  function PingPongEnv(): Env {
    Env(map[0 := Info(4, DMA, None), 1 := Info(4, RAM, None)], map[])
  }

  function PingPongBody(): Stmt {
    Seq(Sync(0), Seq(Start(1), Seq(Read(0), Swap(0, 1))))
  }

  lemma PingPongIsAccepted()
    ensures Check(PingPongEnv(), While(PingPongBody())) == Some(PingPongEnv())
  {
    var e := PingPongEnv();
    var e1 := SetDom(e, 0, RAM);
    var e2 := SetDom(e1, 1, DMA);
    var e3 := e2.(names := SwapNames(e2.names, 0, 1));
    assert Check(e, Sync(0)) == Some(e1);
    assert Check(e1, Start(1)) == Some(e2);
    assert Check(e2, Read(0)) == Some(e2);
    assert SwapGuard(e2, 0, 1);
    assert Check(e2, Swap(0, 1)) == Some(e3);
    assert e3.names == e.names;
    assert e3 == e;
    assert Check(e2, Seq(Read(0), Swap(0, 1))) == Some(e);
    assert Check(e1, Seq(Start(1), Seq(Read(0), Swap(0, 1)))) == Some(e);
    assert Check(e, PingPongBody()) == Some(e);
  }

  // ping_pong_without_swap.phase: the same loop without the swap leaves the
  // buffers in each other's starting domains, and is rejected.
  lemma PingPongWithoutSwapIsRejected()
    ensures Check(PingPongEnv(), While(Seq(Sync(0), Seq(Start(1), Read(0))))) == None
  {
    var e := PingPongEnv();
    var e1 := SetDom(e, 0, RAM);
    var e2 := SetDom(e1, 1, DMA);
    assert Check(e, Sync(0)) == Some(e1);
    assert Check(e1, Start(1)) == Some(e2);
    assert Check(e2, Read(0)) == Some(e2);
    assert Check(e, Seq(Sync(0), Seq(Start(1), Read(0)))) == Some(e2);
    assert e2.names[0].dom != e.names[0].dom;
    assert e2 != e;
  }
}
