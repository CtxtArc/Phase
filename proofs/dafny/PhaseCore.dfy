// PhaseCore.dfy — a mechanized core calculus for PHASE's domain rules.
//
// v0 scope: buffer domains (RAM / DMA / Dead), sync, destroy, sequencing,
// if/else merges and while-loop invariants. The theorem: every program the
// checker accepts never faults at run time, along ANY branch/iteration path,
// and ends in exactly the domains the checker predicted.
//
// Not yet modeled: borrows, split/join, swap/rotate, typestate, registers,
// interrupts, memory contents + device writes. See the roadmap at the bottom.

module PhaseCore {

  datatype Option<T> = None | Some(value: T)

  type BufId = nat

  // ---------------------------------------------------------------------
  // Syntax
  // ---------------------------------------------------------------------

  datatype Stmt =
    | Skip
    | Read(b: BufId)        // CPU reads b
    | Write(b: BufId)       // CPU writes b
    | Start(b: BufId)       // hand b to the DMA engine     (@RAM -> @DMA)
    | Sync(b: BufId)        // wait for the engine, take b back (@DMA -> @RAM)
    | Destroy(b: BufId)     // free b                       (@RAM -> dead)
    | Seq(s1: Stmt, s2: Stmt)
    | If(thn: Stmt, els: Stmt)   // condition abstracted: either branch may run
    | While(body: Stmt)          // may run 0..n times

  // ---------------------------------------------------------------------
  // Static side: a model of PHASE's analyzer
  // ---------------------------------------------------------------------

  datatype Dom = RAM | DMA | Dead
  type Env = map<BufId, Dom>

  function Check(env: Env, s: Stmt): Option<Env>
  {
    match s
    case Skip => Some(env)
    case Read(b) =>
      if b in env && env[b] == RAM then Some(env) else None
    case Write(b) =>
      if b in env && env[b] == RAM then Some(env) else None
    case Start(b) =>
      if b in env && env[b] == RAM then Some(env[b := DMA]) else None
    case Sync(b) =>
      if b in env && env[b] == DMA then Some(env[b := RAM]) else None
    case Destroy(b) =>
      if b in env && env[b] == RAM then Some(env[b := Dead]) else None
    case Seq(s1, s2) =>
      match Check(env, s1)
      case None => None
      case Some(mid) => Check(mid, s2)
    case If(t, e) =>
      // Rule 5: both branches must leave every buffer in the same domain.
      match (Check(env, t), Check(env, e))
      case (Some(a), Some(c)) => if a == c then Some(a) else None
      case _ => None
    case While(body) =>
      // Rule 5: a loop body must leave every buffer as it found it.
      if Check(env, body) == Some(env) then Some(env) else None
  }

  // ---------------------------------------------------------------------
  // Dynamic side: who really owns each buffer at run time
  // ---------------------------------------------------------------------

  datatype Owner = Cpu | Engine | Freed
  datatype State = State(owner: map<BufId, Owner>)
  datatype Outcome = Ok(st: State) | Fault

  predicate CpuOwns(st: State, b: BufId) {
    b in st.owner && st.owner[b] == Cpu
  }

  // Relational big-step semantics. Nondeterministic in If and While, so a
  // theorem about Exec covers every path the program could take.
  least predicate Exec(s: Stmt, st: State, out: Outcome)
  {
    match s
    case Skip => out == Ok(st)
    case Read(b) =>
      out == (if CpuOwns(st, b) then Ok(st) else Fault)
    case Write(b) =>
      out == (if CpuOwns(st, b) then Ok(st) else Fault)
    case Start(b) =>
      out == (if CpuOwns(st, b) then Ok(State(st.owner[b := Engine])) else Fault)
    case Sync(b) =>
      out == (if b in st.owner && st.owner[b] == Engine
              then Ok(State(st.owner[b := Cpu])) else Fault)
    case Destroy(b) =>
      out == (if CpuOwns(st, b) then Ok(State(st.owner[b := Freed])) else Fault)
    case Seq(s1, s2) =>
      (out == Fault && Exec(s1, st, Fault))
      || (exists mid :: Exec(s1, st, Ok(mid)) && Exec(s2, mid, out))
    case If(t, e) =>
      Exec(t, st, out) || Exec(e, st, out)
    case While(body) =>
      out == Ok(st)
      || (out == Fault && Exec(body, st, Fault))
      || (exists mid :: Exec(body, st, Ok(mid)) && Exec(While(body), mid, out))
  }

  // ---------------------------------------------------------------------
  // The link between the two: the checker's environment describes reality
  // ---------------------------------------------------------------------

  function Abs(o: Owner): Dom {
    match o
    case Cpu => RAM
    case Engine => DMA
    case Freed => Dead
  }

  ghost predicate Agrees(env: Env, st: State) {
    env.Keys == st.owner.Keys
    && forall b :: b in env ==> Abs(st.owner[b]) == env[b]
  }

  // ---------------------------------------------------------------------
  // Soundness: accepted programs never fault, on any path
  // ---------------------------------------------------------------------

  least lemma Soundness(s: Stmt, st: State, out: Outcome, env: Env, env': Env)
    requires Exec(s, st, out)
    requires Check(env, s) == Some(env')
    requires Agrees(env, st)
    ensures out.Ok? && Agrees(env', out.st)
  {
    match s
    case Skip =>
    case Read(b) =>
    case Write(b) =>
    case Start(b) =>
      assert env[b] == RAM;
      assert st.owner[b] == Cpu;
    case Sync(b) =>
      assert env[b] == DMA;
      assert st.owner[b] == Engine;
    case Destroy(b) =>
      assert env[b] == RAM;
      assert st.owner[b] == Cpu;
    case Seq(s1, s2) =>
      var mid_env := Check(env, s1).value;
      if out == Fault && Exec(s1, st, Fault) {
        Soundness(s1, st, Fault, env, mid_env);   // contradiction: Fault.Ok?
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
      } else if out == Fault && Exec(body, st, Fault) {
        Soundness(body, st, Fault, env, env);     // contradiction
      } else {
        var mid :| Exec(body, st, Ok(mid)) && Exec(While(body), mid, out);
        Soundness(body, st, Ok(mid), env, env);
        Soundness(While(body), mid, out, env, env);
      }
  }

  // ---------------------------------------------------------------------
  // Rule 3 as a corollary: no function returns while hardware owns a buffer
  // ---------------------------------------------------------------------

  ghost predicate NothingInFlight(env: Env) {
    forall b :: b in env ==> env[b] != DMA
  }

  lemma FunctionExitSafety(body: Stmt, env0: Env, envF: Env, st0: State, out: Outcome)
    requires Check(env0, body) == Some(envF)
    requires NothingInFlight(envF)
    requires Agrees(env0, st0)
    requires Exec(body, st0, out)
    ensures out.Ok?
    ensures forall b :: b in out.st.owner ==> out.st.owner[b] != Engine
  {
    Soundness(body, st0, out, env0, envF);
  }

  // ---------------------------------------------------------------------
  // Sanity checks: the bug-gallery shapes are rejected
  // ---------------------------------------------------------------------

  // use_before_sync.phase
  lemma RejectsUseBeforeSync()
    ensures Check(map[0 := RAM], Seq(Start(0), Read(0))) == None
  {}

  // destroy_while_dma.phase
  lemma RejectsDestroyWhileDma()
    ensures Check(map[0 := RAM], Seq(Start(0), Destroy(0))) == None
  {}

  // branch_domain_disagreement.phase
  lemma RejectsBranchDisagreement()
    ensures Check(map[0 := RAM], If(Start(0), Skip)) == None
  {}

  // a loop that starts a capture each iteration without syncing it
  lemma RejectsLeakyLoop()
    ensures Check(map[0 := RAM], While(Start(0))) == None
  {}

  // a well-formed capture loop is accepted
  lemma AcceptsCaptureLoop()
    ensures Check(map[0 := RAM], While(Seq(Start(0), Seq(Sync(0), Read(0)))))
            == Some(map[0 := RAM])
  {}
}

// ===========================================================================
// Roadmap, in order of how much each step adds
// ===========================================================================
//
// 1. Memory + device interference (the real theorem).
//    Add mem: map<BufId, seq<int>> to State and a DeviceStep that may scribble
//    over any Engine-owned buffer between any two CPU steps. Prove
//    NONINTERFERENCE: what Read observes is the same whatever the device does,
//    i.e. accepted programs are deterministic despite a hostile engine.
//    That is the formal meaning of "no DMA race".
//
// 2. Borrows. Add Borrow/Release with a read/write count per buffer; prove
//    many-readers-xor-one-writer and that nothing is moved/destroyed while
//    borrowed.
//
// 3. Swap / rotate / split / join. Environments become maps over names that
//    can alias halves of one physical buffer; this is where the fuzzer found
//    real bugs, so it is the most valuable to mechanize.
//
// 4. Functions with domain-changing signatures (@RAM -> @DMA), checked
//    per-function and composed — the modular soundness argument the README
//    relies on.
//
// 5. Interrupts and `wait E for b`: an interleaving semantics with one
//    handler level, and the critical-section rule for read-modify-write.
//
// 6. Close the gap to the Rust analyzer: export Check to Rust/Go/Python
//    (Dafny compiles it), and differential-test it against `phase check`
//    using phase-fuzz's generator. Any disagreement is a bug in one of them.
