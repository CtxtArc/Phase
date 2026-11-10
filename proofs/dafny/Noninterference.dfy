// Noninterference.dfy — step 1 of the roadmap: "no DMA race", formally.
//
// PhaseCore.dfy proves that accepted programs respect ownership. This file
// proves what that buys you physically. The machine here has REAL memory and
// a HOSTILE DMA engine:
//
//   * between any two CPU steps, the engine may write anything it likes into
//     every buffer it currently owns (a "scribble" — partial transfers, torn
//     words, whatever timing produces);
//   * hardware does NOT stop the CPU from reading an in-flight buffer: such a
//     read just returns whatever the engine has written so far;
//   * when a transfer completes (`Sync`), the buffer holds the transfer's
//     result, which is input to the program.
//
// Theorem (Noninterference): for a program the checker accepts, two runs that
// see the same inputs and take the same branches, but face DIFFERENT engine
// scribbles, read exactly the same values, in the same order. In other words
// the CPU can never observe a transfer in progress: to the program, every
// transfer looks as if it happened instantaneously at its `Sync`.
//
// `InterferenceWithoutTheChecker` shows the theorem is not vacuous: a program
// the checker rejects (read before sync) does see the scribbles.

include "PhaseCore.dfy"

module PhaseNoninterference {
  import opened PhaseCore

  // ---------------------------------------------------------------------
  // The physical machine
  // ---------------------------------------------------------------------

  // owner: who holds each buffer.  mem: its contents (one word per buffer is
  // enough: the argument is per buffer).  clock: one tick per step.
  // trace: every value the CPU has read, in order — the observation.
  datatype M = M(owner: map<BufId, Owner>, mem: map<BufId, int>,
                 clock: nat, trace: seq<int>)

  // The world, apart from the engine's in-flight writes: which way each
  // branch goes, and the values written by the CPU and delivered at Sync.
  datatype World = World(path: nat -> bool, input: nat -> int)

  // The engine's in-flight writes: at time t it writes scr(t)(b) into every
  // buffer b it owns.  Completely arbitrary.
  type Scribble = nat -> BufId -> int

  datatype Res = Done(m: M) | Crash | NoFuel

  function Tick(m: M): M { m.(clock := m.clock + 1) }

  function Scrib(m: M, scr: Scribble): M {
    m.(mem := map b | b in m.mem.Keys ::
                if b in m.owner && m.owner[b] == Engine
                then scr(m.clock)(b) else m.mem[b])
  }

  // One CPU step on real hardware.  Crash = use after free, double free,
  // freeing a buffer the engine is writing, or waiting on a transfer that was
  // never started.  Reading or writing an IN-FLIGHT buffer is not a crash:
  // the hardware lets it happen, and that is exactly the race.
  function Prim(s: Stmt, m: M, w: World): Res {
    match s
    case Skip => Done(m)
    case Read(b) =>
      if b in m.owner && m.owner[b] != Freed && b in m.mem
      then Done(m.(trace := m.trace + [m.mem[b]])) else Crash
    case Write(b) =>
      if b in m.owner && m.owner[b] != Freed && b in m.mem
      then Done(m.(mem := m.mem[b := w.input(m.clock)])) else Crash
    case Start(b) =>
      if b in m.owner && m.owner[b] == Cpu
      then Done(m.(owner := m.owner[b := Engine])) else Crash
    case Sync(b) =>
      if b in m.owner && m.owner[b] == Engine && b in m.mem
      then Done(m.(owner := m.owner[b := Cpu], mem := m.mem[b := w.input(m.clock)]))
      else Crash
    case Destroy(b) =>
      if b in m.owner && m.owner[b] == Cpu
      then Done(m.(owner := m.owner[b := Freed])) else Crash
    case _ => Crash
  }

  // A deterministic run, given the world, the engine's scribbles and a fuel
  // bound for loops.  The engine gets a turn before every primitive step.
  function Run(s: Stmt, m: M, w: World, scr: Scribble, fuel: nat): Res
    decreases fuel, s
  {
    match s
    case Seq(s1, s2) =>
      (match Run(s1, m, w, scr, fuel)
       case Done(m1) => Run(s2, m1, w, scr, fuel)
       case r => r)
    case If(t, e) =>
      if w.path(m.clock) then Run(t, Tick(m), w, scr, fuel)
      else Run(e, Tick(m), w, scr, fuel)
    case While(body) =>
      if fuel == 0 then NoFuel
      else if !w.path(m.clock) then Done(Tick(m))
      else
        (match Run(body, Tick(m), w, scr, fuel)
         case Done(m1) => Run(While(body), m1, w, scr, fuel - 1)
         case r => r)
    case Skip => Prim(s, Tick(Scrib(m, scr)), w)
    case Read(_) => Prim(s, Tick(Scrib(m, scr)), w)
    case Write(_) => Prim(s, Tick(Scrib(m, scr)), w)
    case Start(_) => Prim(s, Tick(Scrib(m, scr)), w)
    case Sync(_) => Prim(s, Tick(Scrib(m, scr)), w)
    case Destroy(_) => Prim(s, Tick(Scrib(m, scr)), w)
  }

  // ---------------------------------------------------------------------
  // Invariants
  // ---------------------------------------------------------------------

  // The checker's environment describes the machine's ownership.
  ghost predicate Inv(env: Env, m: M) {
    env.Keys == m.owner.Keys
    && m.mem.Keys == m.owner.Keys
    && forall b :: b in env ==> Abs(m.owner[b]) == env[b]
  }

  // Two machines agree on everything except what the engine is in the middle
  // of writing.
  ghost predicate Rel(m1: M, m2: M) {
    m1.owner == m2.owner
    && m1.clock == m2.clock
    && m1.trace == m2.trace
    && m1.mem.Keys == m2.mem.Keys
    && forall b :: b in m1.mem && b in m1.owner && m1.owner[b] != Engine
                   ==> m1.mem[b] == m2.mem[b]
  }

  lemma EngineTurnPreserves(env: Env, m1: M, m2: M, scr1: Scribble, scr2: Scribble)
    requires Inv(env, m1) && Inv(env, m2) && Rel(m1, m2)
    ensures Inv(env, Tick(Scrib(m1, scr1)))
    ensures Inv(env, Tick(Scrib(m2, scr2)))
    ensures Rel(Tick(Scrib(m1, scr1)), Tick(Scrib(m2, scr2)))
  {
    var n1 := Scrib(m1, scr1);
    var n2 := Scrib(m2, scr2);
    assert n1.mem.Keys == m1.mem.Keys;
    assert n2.mem.Keys == m2.mem.Keys;
    forall b | b in n1.mem && b in n1.owner && n1.owner[b] != Engine
      ensures n1.mem[b] == n2.mem[b]
    {
      assert n1.mem[b] == m1.mem[b];
      assert n2.mem[b] == m2.mem[b];
    }
  }

  // ---------------------------------------------------------------------
  // The theorem
  // ---------------------------------------------------------------------

  lemma Noninterference(s: Stmt, env: Env, env': Env, m1: M, m2: M,
                        w: World, scr1: Scribble, scr2: Scribble, fuel: nat)
    requires Check(env, s) == Some(env')
    requires Inv(env, m1) && Inv(env, m2) && Rel(m1, m2)
    ensures var r1 := Run(s, m1, w, scr1, fuel);
            var r2 := Run(s, m2, w, scr2, fuel);
            !r1.Crash? && !r2.Crash?
            && (r1.Done? <==> r2.Done?)
            && (r1.Done? ==> Inv(env', r1.m) && Inv(env', r2.m) && Rel(r1.m, r2.m))
    decreases fuel, s
  {
    match s
    case Seq(s1, s2) =>
      var mid := Check(env, s1).value;
      Noninterference(s1, env, mid, m1, m2, w, scr1, scr2, fuel);
      var r1 := Run(s1, m1, w, scr1, fuel);
      var r2 := Run(s1, m2, w, scr2, fuel);
      if r1.Done? {
        Noninterference(s2, mid, env', r1.m, r2.m, w, scr1, scr2, fuel);
      }
    case If(t, e) =>
      assert Check(env, t) == Some(env');
      assert Check(env, e) == Some(env');
      assert m1.clock == m2.clock;
      if w.path(m1.clock) {
        Noninterference(t, env, env', Tick(m1), Tick(m2), w, scr1, scr2, fuel);
      } else {
        Noninterference(e, env, env', Tick(m1), Tick(m2), w, scr1, scr2, fuel);
      }
    case While(body) =>
      assert env' == env;
      assert Check(env, body) == Some(env);
      assert m1.clock == m2.clock;
      if fuel == 0 {
      } else if !w.path(m1.clock) {
      } else {
        Noninterference(body, env, env, Tick(m1), Tick(m2), w, scr1, scr2, fuel);
        var r1 := Run(body, Tick(m1), w, scr1, fuel);
        var r2 := Run(body, Tick(m2), w, scr2, fuel);
        if r1.Done? {
          Noninterference(While(body), env, env, r1.m, r2.m, w, scr1, scr2, fuel - 1);
        }
      }
    case Skip =>
      EngineTurnPreserves(env, m1, m2, scr1, scr2);
    case Read(b) =>
      EngineTurnPreserves(env, m1, m2, scr1, scr2);
      var n1 := Tick(Scrib(m1, scr1));
      var n2 := Tick(Scrib(m2, scr2));
      assert env[b] == RAM;
      assert n1.owner[b] == Cpu;
      assert n1.mem[b] == n2.mem[b];
    case Write(b) =>
      EngineTurnPreserves(env, m1, m2, scr1, scr2);
      var n1 := Tick(Scrib(m1, scr1));
      assert env[b] == RAM;
      assert n1.owner[b] == Cpu;
    case Start(b) =>
      EngineTurnPreserves(env, m1, m2, scr1, scr2);
      var n1 := Tick(Scrib(m1, scr1));
      assert env[b] == RAM;
      assert n1.owner[b] == Cpu;
    case Sync(b) =>
      EngineTurnPreserves(env, m1, m2, scr1, scr2);
      var n1 := Tick(Scrib(m1, scr1));
      assert env[b] == DMA;
      assert n1.owner[b] == Engine;
    case Destroy(b) =>
      EngineTurnPreserves(env, m1, m2, scr1, scr2);
      var n1 := Tick(Scrib(m1, scr1));
      var n2 := Tick(Scrib(m2, scr2));
      assert env[b] == RAM;
      assert n1.owner[b] == Cpu;
      assert n1.mem[b] == n2.mem[b];
  }

  // The headline, stated on what the program observes.
  lemma AcceptedProgramsCannotSeeTransfersInProgress(
      s: Stmt, env: Env, env': Env, m: M,
      w: World, scr1: Scribble, scr2: Scribble, fuel: nat)
    requires Check(env, s) == Some(env')
    requires Inv(env, m)
    ensures var r1 := Run(s, m, w, scr1, fuel);
            var r2 := Run(s, m, w, scr2, fuel);
            r1.Done? && r2.Done? ==> r1.m.trace == r2.m.trace
  {
    Noninterference(s, env, env', m, m, w, scr1, scr2, fuel);
  }

  // ---------------------------------------------------------------------
  // The theorem has teeth: without the checker, the scribbles are visible
  // ---------------------------------------------------------------------

  function M0(): M { M(map[0 := Cpu], map[0 := 0], 0, []) }
  function W0(): World { World((t: nat) => false, (t: nat) => 0) }

  // use_before_sync.phase: start a capture, read the buffer straight away.
  lemma ReadDuringTransferSeesTheEngine(scr: Scribble)
    ensures Check(map[0 := RAM], Seq(Start(0), Read(0))) == None
    ensures Run(Seq(Start(0), Read(0)), M0(), W0(), scr, 0).Done?
    ensures Run(Seq(Start(0), Read(0)), M0(), W0(), scr, 0).m.trace == [scr(1)(0)]
  {
    var a := Tick(Scrib(M0(), scr));
    assert a.owner == map[0 := Cpu];
    var m1 := a.(owner := a.owner[0 := Engine]);
    assert Run(Start(0), M0(), W0(), scr, 0) == Done(m1);
    var b := Tick(Scrib(m1, scr));
    assert m1.clock == 1;
    assert b.mem[0] == scr(1)(0);
    assert Run(Read(0), m1, W0(), scr, 0) == Done(b.(trace := [scr(1)(0)]));
  }

  lemma InterferenceWithoutTheChecker()
    ensures Run(Seq(Start(0), Read(0)), M0(), W0(), (t: nat) => (b: nat) => 1, 0).m.trace
         != Run(Seq(Start(0), Read(0)), M0(), W0(), (t: nat) => (b: nat) => 2, 0).m.trace
  {
    ReadDuringTransferSeesTheEngine((t: nat) => (b: nat) => 1);
    ReadDuringTransferSeesTheEngine((t: nat) => (b: nat) => 2);
  }
}
