" Vim syntax file for PHASE (.phase). Install: copy editors/vim/ into
" ~/.vim/ (or ~/.config/nvim/), or add it to 'runtimepath'.
if exists("b:current_syntax")
  finish
endif

syn keyword phaseDecl       fn extern let entity state
syn keyword phaseControl    if else while return
syn keyword phaseOwnership  move sync borrow release destroy swap rotate split join into
syn keyword phaseAccess     read write
syn keyword phaseBuffer     buffer
syn keyword phaseType       u8 u16 u32 u64 i8 i16 i32 i64 f32 f64 bool
syn keyword phaseBool       true false
syn keyword phaseBuiltin    volatile_read volatile_write
syn match   phaseInterrupt  "\<interrupt\ze\s\+fn\>"
syn match   phaseInterrupt  "@\s*\zsirq\>"
syn match   phaseInterrupt  "^\s*\zsevent\ze\s\+\h\w*\s*;"
syn match   phaseConst      "^\s*\zsconst\ze\s\+\h\w*\s*:"
syn match   phaseInterrupt  "^\s*\zs\(signal\|wait\)\ze\s\+\h\w*\s*;"
syn match   phaseInterrupt  "^\s*\zswait\ze\s\+\h\w*\s\+for\s\+\h\w*\s*;"
syn match   phaseInterrupt  "^\s*wait\s\+\h\w*\s\+\zsfor\ze\s\+\h\w*\s*;"
syn match   phaseInterrupt  "^\s*\zscritical\ze\s*{"
syn match   phaseDevice     "^\s*\zsdevice\ze\s\+\h"
syn match   phaseDevice     "\(^\s*device\s\+\)\@<=type\ze\s\+\h"
syn match   phaseImport     "^\s*\zsimport\ze\s\+\""
syn match   phaseBit        "\<bits\=\>"
syn match   phaseDomain     "@\(RAM\|STACK\|DMA\|MMIO\|DEVICE\)\>"
syn match   phaseNumber     "\<\d\+\(\.\d\+\)\=\>"
syn match   phaseNumber     "\<0[xX][0-9A-Fa-f_]\+\>"
syn match   phaseOperator   "->\|\.\.="
syn region  phaseString     start=+"+ skip=+\\.+ end=+"+
syn match   phaseComment    "//.*$" contains=@Spell
syn region  phaseComment    start="/\*" end="\*/" contains=@Spell

hi def link phaseDecl       Keyword
hi def link phaseDevice     Keyword
hi def link phaseImport     Include
hi def link phaseInterrupt  Statement
hi def link phaseControl    Conditional
hi def link phaseOwnership  Statement
hi def link phaseAccess     StorageClass
hi def link phaseBuffer     Type
hi def link phaseType       Type
hi def link phaseBool       Boolean
hi def link phaseBuiltin    Function
hi def link phaseBit        Keyword
hi def link phaseDomain     Constant
hi def link phaseNumber     Number
hi def link phaseOperator   Operator
hi def link phaseConst      StorageClass
hi def link phaseString     String
hi def link phaseComment    Comment

let b:current_syntax = "phase"
