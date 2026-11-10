// Borrows.dfy — rule 2: many readers or one writer.
//
// Adds borrows to the core calculus. A borrow is a second path to a buffer
// (a pointer handed to a C kernel, a view, a slice). What can go wrong with
// a second path is that someone else changes the data behind its back: a
// reader sees a value change under it, or a writer that assumed it had the
// buffer to itself (C `restrict`) finds its data was modified meanwhile.
//
// The run-time semantics makes that observable. Every borrow remembers the
// value it is entitled to see (its snapshot, updated by its own writes);
// using a borrow whose buffer no longer matches its snapshot is `Stale`.
// The hardware does NOT stop the owner from writing a borrowed buffer, so
// Stale is reachable by programs the checker rejects (`WriteUnderAReader`).
//
// Theorem (Soundness): a program the checker accepts never faults and never
// goes Stale, on any path. Every borrow sees exactly the data it would see
// if it were the only way to reach its buffer.
//
// Corollary (NoBorrowOutlivesItsFunction): if the checker says no borrow is
// live at the end, none is live at run time.

module PhaseBorrows {

  datatype Option<T> = None | Some(value: T)

  type BufId = nat
  type RefId = nat

  datatype Kind = Shared | Mut
  datatype Ref = Ref(buf: BufId, kind: Kind)
  type Refs = map<RefId, Ref>

  // ---------------------------------------------------------------------
  // Syntax
  // ---------------------------------------------------------------------

  datatype Stmt =
    | Skip
    | Read(b: BufId)
    | Write(b: BufId, v: int)
    | Start(b: BufId)
    | Sync(b: BufId)
    | Destroy(b: BufId)
    | Borrow(r: RefId, b: BufId, k: Kind)   // `borrow` / `borrow mut`
    | ReadVia(r: RefId)                     // read through a borrow
    | WriteVia(r: RefId, v: int)            // write through a borrow
    | Release(r: RefId)                     // end the borrow
    | Seq(s1: Stmt, s2: Stmt)
    | If(thn: Stmt, els: Stmt)
    | While(body: Stmt)

  // ---------------------------------------------------------------------
  // Static side: the analyzer's domain and borrow rules
  // ---------------------------------------------------------------------

  datatype Dom = RAM | DMA | Dead
  datatype Env = Env(dom: map<BufId, Dom>, refs: Refs)

  predicate Borrowed(refs: Refs, b: BufId) {
    exists r :: r in refs && refs[r].buf == b
  }

  predicate MutBorrowed(refs: Refs, b: BufId) {
    exists r :: r in refs && refs[r].buf == b && refs[r].kind == Mut
  }

  predicate IsRam(env: Env, b: BufId) {
    b in env.dom && env.dom[b] == RAM
  }

  function Check(env: Env, s: Stmt): Option<Env>
    decreases s
  {
    match s
    case Skip => Some(env)
    case Read(b) =>
      // the owner may read alongside readers, never alongside a writer
      if IsRam(env, b) && !MutBorrowed(env.refs, b) then Some(env) else None
    case Write(b, _) =>
      // while a buffer is borrowed it can't be written...
      if IsRam(env, b) && !Borrowed(env.refs, b) then Some(env) else None
    case Start(b) =>
      // ...moved...
      if IsRam(env, b) && !Borrowed(env.refs, b)
      then Some(env.(dom := env.dom[b := DMA])) else None
    case Sync(b) =>
      if b in env.dom && env.dom[b] == DMA
      then Some(env.(dom := env.dom[b := RAM])) else None
    case Destroy(b) =>
      // ...or freed
      if IsRam(env, b) && !Borrowed(env.refs, b)
      then Some(env.(dom := env.dom[b := Dead])) else None
    case Borrow(r, b, k) =>
      // many readers or one writer
      if r !in env.refs && IsRam(env, b)
         && (if k == Mut then !Borrowed(env.refs, b) else !MutBorrowed(env.refs, b))
      then Some(env.(refs := env.refs[r := Ref(b, k)])) else None
    case ReadVia(r) =>
      // released borrows are dead
      if r in env.refs then Some(env) else None
    case WriteVia(r, _) =>
      // read-only data is never written
      if r in env.refs && env.refs[r].kind == Mut then Some(env) else None
    case Release(r) =>
      if r in env.refs then Some(env.(refs := env.refs - {r})) else None
    case Seq(s1, s2) =>
      (match Check(env, s1)
       case None => None
       case Some(mid) => Check(mid, s2))
    case If(t, e) =>
      // branches must agree on domains AND on which borrows are live
      (match (Check(env, t), Check(env, e))
       case (Some(a), Some(c)) => if a == c then Some(a) else None
       case _ => None)
    case While(body) =>
      if Check(env, body) == Some(env) then Some(env) else None
  }

  // ---------------------------------------------------------------------
  // Dynamic side
  // ---------------------------------------------------------------------

  datatype Owner = Cpu | Engine | Freed

  // refs: the live borrows.  snap: for each, the value it is entitled to see.
  datatype State = State(owner: map<BufId, Owner>, mem: map<BufId, int>,
                         refs: Refs, snap: map<RefId, int>)

  datatype Outcome = Ok(st: State) | Fault | Stale

  predicate CpuOwns(st: State, b: BufId) {
    b in st.owner && st.owner[b] == Cpu && b in st.mem
  }

  predicate Usable(st: State, r: RefId) {
    r in st.refs && r in st.snap && CpuOwns(st, st.refs[r].buf)
  }

  least predicate Exec(s: Stmt, st: State, out: Outcome)
  {
    match s
    case Skip => out == Ok(st)
    case Read(b) =>
      out == (if CpuOwns(st, b) then Ok(st) else Fault)
    case Write(b, v) =>
      // the hardware happily lets the owner write a borrowed buffer
      out == (if CpuOwns(st, b) then Ok(st.(mem := st.mem[b := v])) else Fault)
    case Start(b) =>
      out == (if CpuOwns(st, b) then Ok(st.(owner := st.owner[b := Engine])) else Fault)
    case Sync(b) =>
      out == (if b in st.owner && st.owner[b] == Engine
              then Ok(st.(owner := st.owner[b := Cpu])) else Fault)
    case Destroy(b) =>
      out == (if CpuOwns(st, b) then Ok(st.(owner := st.owner[b := Freed])) else Fault)
    case Borrow(r, b, k) =>
      out == (if r !in st.refs && CpuOwns(st, b)
              then Ok(st.(refs := st.refs[r := Ref(b, k)],
                          snap := st.snap[r := st.mem[b]]))
              else Fault)
    case ReadVia(r) =>
      out == (if Usable(st, r)
              then (if st.mem[st.refs[r].buf] == st.snap[r] then Ok(st) else Stale)
              else Fault)
    case WriteVia(r, v) =>
      out == (if Usable(st, r)
              then (if st.mem[st.refs[r].buf] == st.snap[r]
                    then Ok(st.(mem := st.mem[st.refs[r].buf := v],
                                snap := st.snap[r := v]))
                    else Stale)
              else Fault)
    case Release(r) =>
      out == (if r in st.refs
              then Ok(st.(refs := st.refs - {r}, snap := st.snap - {r}))
              else Fault)
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
  // The invariant
  // ---------------------------------------------------------------------

  function Abs(o: Owner): Dom {
    match o
    case Cpu => RAM
    case Engine => DMA
    case Freed => Dead
  }

  // Exclusive: a Mut borrow is the only borrow of its buffer.
  ghost predicate Exclusive(refs: Refs) {
    forall r, q :: r in refs && q in refs && r != q && refs[r].buf == refs[q].buf
                   ==> refs[r].kind == Shared && refs[q].kind == Shared
  }

  ghost predicate Inv(env: Env, st: State) {
    env.dom.Keys == st.owner.Keys
    && st.mem.Keys == st.owner.Keys
    && (forall b :: b in env.dom ==> Abs(st.owner[b]) == env.dom[b])
    && env.refs == st.refs
    && st.snap.Keys == st.refs.Keys
    // borrows only ever point at CPU-owned buffers
    && (forall r :: r in st.refs ==> IsRam(env, st.refs[r].buf))
    // every borrow still sees what it is entitled to see
    && (forall r :: r in st.refs ==> st.mem[st.refs[r].buf] == st.snap[r])
    && Exclusive(st.refs)
  }

  lemma NotBorrowed(refs: Refs, b: BufId)
    requires !Borrowed(refs, b)
    ensures forall r :: r in refs ==> refs[r].buf != b
  {
    forall r | r in refs ensures refs[r].buf != b {
      if refs[r].buf == b { assert Borrowed(refs, b); }
    }
  }

  lemma NotMutBorrowed(refs: Refs, b: BufId)
    requires !MutBorrowed(refs, b)
    ensures forall r :: r in refs && refs[r].buf == b ==> refs[r].kind == Shared
  {
    forall r | r in refs && refs[r].buf == b ensures refs[r].kind == Shared {
      if refs[r].kind == Mut { assert MutBorrowed(refs, b); }
    }
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
    case Read(b) =>
      assert IsRam(env, b);
      assert st.owner[b] == Cpu;
    case Write(b, v) =>
      assert IsRam(env, b) && !Borrowed(env.refs, b);
      assert st.owner[b] == Cpu;
      NotBorrowed(st.refs, b);
      var st' := st.(mem := st.mem[b := v]);
      assert out == Ok(st');
      forall r | r in st'.refs ensures st'.mem[st'.refs[r].buf] == st'.snap[r] {
        assert st.refs[r].buf != b;
      }
    case Start(b) =>
      assert IsRam(env, b) && !Borrowed(env.refs, b);
      assert st.owner[b] == Cpu;
      NotBorrowed(st.refs, b);
    case Sync(b) =>
      assert env.dom[b] == DMA;
      assert st.owner[b] == Engine;
      forall r | r in st.refs ensures st.refs[r].buf != b {
        assert IsRam(env, st.refs[r].buf);
      }
    case Destroy(b) =>
      assert IsRam(env, b) && !Borrowed(env.refs, b);
      assert st.owner[b] == Cpu;
      NotBorrowed(st.refs, b);
    case Borrow(r, b, k) =>
      assert r !in env.refs && IsRam(env, b);
      assert st.owner[b] == Cpu;
      var refs' := st.refs[r := Ref(b, k)];
      if k == Mut {
        assert !Borrowed(env.refs, b);
        NotBorrowed(st.refs, b);
      } else {
        assert !MutBorrowed(env.refs, b);
        NotMutBorrowed(st.refs, b);
      }
      forall p, q | p in refs' && q in refs' && p != q && refs'[p].buf == refs'[q].buf
        ensures refs'[p].kind == Shared && refs'[q].kind == Shared
      {
        if p != r && q != r {
          assert st.refs[p].buf == st.refs[q].buf;
        }
      }
      assert Exclusive(refs');
    case ReadVia(r) =>
      assert r in st.refs;
      assert IsRam(env, st.refs[r].buf);
      assert st.owner[st.refs[r].buf] == Cpu;
    case WriteVia(r, v) =>
      assert r in st.refs && st.refs[r].kind == Mut;
      var b := st.refs[r].buf;
      assert IsRam(env, b);
      assert st.owner[b] == Cpu;
      // the writer is alone on its buffer, so no other borrow's view changes
      forall q | q in st.refs && q != r ensures st.refs[q].buf != b {
        if st.refs[q].buf == b {
          assert st.refs[r].kind == Shared;
        }
      }
      var st' := st.(mem := st.mem[b := v], snap := st.snap[r := v]);
      assert out == Ok(st');
      forall q | q in st'.refs ensures st'.mem[st'.refs[q].buf] == st'.snap[q] {
        if q != r { assert st.refs[q].buf != b; }
      }
    case Release(r) =>
      assert r in st.refs;
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

  // No function returns, and no block ends, with a borrow still live.
  lemma NoBorrowOutlivesItsFunction(body: Stmt, env0: Env, envF: Env,
                                    st0: State, out: Outcome)
    requires Check(env0, body) == Some(envF)
    requires envF.refs == map[]
    requires Inv(env0, st0)
    requires Exec(body, st0, out)
    ensures out.Ok? && out.st.refs == map[]
  {
    Soundness(body, st0, out, env0, envF);
  }

  // ---------------------------------------------------------------------
  // The theorem has teeth: the rejected shapes really do go wrong
  // ---------------------------------------------------------------------

  function St0(): State { State(map[0 := Cpu], map[0 := 0], map[], map[]) }
  function Env0(): Env { Env(map[0 := RAM], map[]) }

  // use_owner_while_borrowed.phase: hand out a read view, then write the
  // buffer behind its back, then read through the view.
  function WriteUnderAReader(): Stmt {
    Seq(Borrow(0, 0, Shared), Seq(Write(0, 5), ReadVia(0)))
  }

  lemma WriteUnderAReaderIsRejected()
    ensures Check(Env0(), WriteUnderAReader()) == None
  {
    var e1 := Env0().(refs := map[0 := Ref(0, Shared)]);
    assert Check(Env0(), Borrow(0, 0, Shared)) == Some(e1);
    assert Borrowed(e1.refs, 0);
    assert Check(e1, Write(0, 5)) == None;
  }

  lemma WriteUnderAReaderGoesStale()
    ensures Exec(WriteUnderAReader(), St0(), Stale)
  {
    var s0 := St0();
    var s1 := s0.(refs := s0.refs[0 := Ref(0, Shared)], snap := s0.snap[0 := s0.mem[0]]);
    assert Exec(Borrow(0, 0, Shared), s0, Ok(s1));
    var s2 := s1.(mem := s1.mem[0 := 5]);
    assert Exec(Write(0, 5), s1, Ok(s2));
    assert s2.mem[0] == 5 && s2.snap[0] == 0;
    assert Exec(ReadVia(0), s2, Stale);
    assert Exec(Seq(Write(0, 5), ReadVia(0)), s1, Stale);
  }

  // write_borrow_conflict.phase: two writers at once.
  lemma TwoWritersAreRejected()
    ensures Check(Env0(), Seq(Borrow(0, 0, Mut), Borrow(1, 0, Mut))) == None
  {
    var e1 := Env0().(refs := map[0 := Ref(0, Mut)]);
    assert Check(Env0(), Borrow(0, 0, Mut)) == Some(e1);
    assert Borrowed(e1.refs, 0);
  }

  // write_through_read_borrow.phase
  lemma WriteThroughReadBorrowIsRejected()
    ensures Check(Env0(), Seq(Borrow(0, 0, Shared), WriteVia(0, 1))) == None
  {
    var e1 := Env0().(refs := map[0 := Ref(0, Shared)]);
    assert Check(Env0(), Borrow(0, 0, Shared)) == Some(e1);
  }

  // use_after_release.phase
  lemma UseAfterReleaseIsRejected()
    ensures Check(Env0(), Seq(Borrow(0, 0, Shared), Seq(Release(0), ReadVia(0)))) == None
  {
    var e1 := Env0().(refs := map[0 := Ref(0, Shared)]);
    assert Check(Env0(), Borrow(0, 0, Shared)) == Some(e1);
    assert e1.refs - {0} == map[];
    assert Check(e1, Release(0)) == Some(Env0());
  }

  // move_while_borrowed.phase
  lemma StartWhileBorrowedIsRejected()
    ensures Check(Env0(), Seq(Borrow(0, 0, Shared), Start(0))) == None
  {
    var e1 := Env0().(refs := map[0 := Ref(0, Shared)]);
    assert Check(Env0(), Borrow(0, 0, Shared)) == Some(e1);
    assert Borrowed(e1.refs, 0);
  }

  // Many readers are fine.
  lemma TwoReadersAreAccepted()
    ensures Check(Env0(), Seq(Borrow(0, 0, Shared),
                          Seq(Borrow(1, 0, Shared),
                          Seq(ReadVia(0), Seq(ReadVia(1),
                          Seq(Release(0), Release(1)))))))
            == Some(Env0())
  {
    var e1 := Env0().(refs := map[0 := Ref(0, Shared)]);
    var e2 := e1.(refs := e1.refs[1 := Ref(0, Shared)]);
    var e3 := e2.(refs := e2.refs - {0});
    assert e3.refs - {1} == map[];
    assert Check(Env0(), Borrow(0, 0, Shared)) == Some(e1);
    assert !MutBorrowed(e1.refs, 0);
    assert Check(e1, Borrow(1, 0, Shared)) == Some(e2);
    assert Check(e2, Release(0)) == Some(e3);
    assert Check(e3, Release(1)) == Some(Env0());
    assert Check(e2, Seq(Release(0), Release(1))) == Some(Env0());
    assert Check(e2, Seq(ReadVia(1), Seq(Release(0), Release(1)))) == Some(Env0());
    assert Check(e2, Seq(ReadVia(0), Seq(ReadVia(1), Seq(Release(0), Release(1)))))
           == Some(Env0());
  }
}
