// Blocking state for agent chat when the Tauri build's startup Node/adapter
// check (Chunk 4, spec #48) fails. Deliberately NOT `CrashSurface`/
// `ErrorBoundary` reuse — those two recovery actions ("Ask Hearth to
// repair", "Undo latest") both assume a working agent runtime, which is
// exactly what's broken here (decision from grilling ticket #47). Rendered
// wherever `ChatView` would otherwise mount its message stream + composer —
// not a full-app overlay, since self-mod/terminal/every other subsystem
// keeps working normally (spec #26's git-availability precedent).

import { useState } from 'react'
import type { AgentRuntimeStatus } from '../../electron/shared/protocol'
import { Icon } from './Icon'

type BlockingStatus = Exclude<AgentRuntimeStatus, { status: 'ok' }>

function describe(status: BlockingStatus): { title: string; detail: string } {
  if (status.status === 'node-missing') {
    return {
      title: 'Agent chat needs Node.js',
      detail: 'Hearth runs agent-chat backends through a system-installed Node.js, which isn’t on your PATH. Install Node.js (nodejs.org), then check again.',
    }
  }
  return {
    title: 'Agent chat is missing a required package',
    detail: `Hearth's agent-chat runtime couldn't find ${status.package}. Try reinstalling or repairing the Hearth install, then check again.`,
  }
}

export function AgentRuntimeUnavailable({
  status,
  onStatusChange,
}: {
  status: BlockingStatus
  onStatusChange: (status: AgentRuntimeStatus) => void
}) {
  const [checking, setChecking] = useState(false)
  const { title, detail } = describe(status)

  const recheck = async () => {
    setChecking(true)
    try {
      onStatusChange(await window.hearth.agentRuntime.recheck())
    } finally {
      setChecking(false)
    }
  }

  return (
    <div className="chat-col" data-screen-label="Chat">
      <div className="chat-scroll scroll">
        <div className="chat-wrap">
          <div className="chat-empty">
            <span className="flame">
              <Icon name="warning" fill />
            </span>
            <h3>{title}</h3>
            <p>{detail}</p>
            <button className="btn" disabled={checking} onClick={() => void recheck()}>
              {checking ? 'Checking…' : 'Check again'}
            </button>
          </div>
        </div>
      </div>
    </div>
  )
}
