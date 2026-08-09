// Phase 7 (tracking issue #27) real-invoke() smoke coverage for the six
// subsystems added this phase (git panel, files tab, skills panel,
// personality, memory, routines) plus the trivial about namespace. Same
// motivation as Hearth#54: unit tests can't catch a command missing from
// build.rs's ACL list or capabilities/default.json — only a real invoke()
// through the actual running app can, and every one of Phase 6's four real
// bugs was found exactly this way. One read-only call per namespace is
// enough to prove the command is reachable; the parsing/persistence logic
// itself already has full Rust #[test] coverage (git_panel.rs,
// fs_commands.rs, skills/list.rs, soul/*.rs, routines/*.rs).

import { expect } from '@wdio/globals'

describe('Phase 7 IPC surface is reachable through the real invoke() path', () => {
  it('git.status resolves without an ACL rejection', async () => {
    const status = await browser.execute(() => window.hearth.git.status())
    expect(status).toHaveProperty('files')
  })

  it('files.list resolves without an ACL rejection', async () => {
    const entries = await browser.execute(() => window.hearth.files.list(undefined, ''))
    expect(Array.isArray(entries)).toBe(true)
  })

  it('skills.list resolves without an ACL rejection', async () => {
    const result = await browser.execute(() => window.hearth.skills.list())
    expect(result).toHaveProperty('skills')
    expect(result).toHaveProperty('commands')
  })

  it('personality.get resolves without an ACL rejection', async () => {
    const config = await browser.execute(() => window.hearth.personality.get())
    expect(config).toHaveProperty('length')
  })

  it('memory.get resolves without an ACL rejection', async () => {
    const memory = await browser.execute(() => window.hearth.memory.get())
    expect(typeof memory).toBe('string')
  })

  it('routines.list resolves without an ACL rejection', async () => {
    const routines = await browser.execute(() => window.hearth.routines.list())
    expect(Array.isArray(routines)).toBe(true)
  })

  it('about.info resolves without an ACL rejection', async () => {
    const info = await browser.execute(() => window.hearth.about.info())
    expect(info).toHaveProperty('app')
  })
})
