import { useState, useEffect } from 'react'
import { ThinkingOrb } from 'thinking-orbs'
import { listen } from '@tauri-apps/api/event'
import { invoke } from '@tauri-apps/api/core'

type OrbState = "working" | "searching" | "solving" | "listening" | "connecting" | "weaving" | "composing" | "breathing" | "shaping";

type PipelineStage = "model-missing" | "transcribing" | "thinking" | "running-tool" | "confirmation-required" | "speaking" | "answer" | "error" | "idle" | "wake-followup" | "listening" | "latency";
type PipelineEvent = { stage: PipelineStage; detail?: string };
type PushToTalkEvent = { active: boolean };
type TaskStatus = "queued" | "running" | "completed" | "failed";
type TaskEvent = { task_id: string; status: TaskStatus; description: string; result?: string };
type BackgroundTask = { id: string; description: string; status: TaskStatus; result?: string };

function App() {
  const [orbState, setOrbState] = useState<OrbState>("listening")
  const [isMuted, setIsMuted] = useState(false)
  const [displayText, setDisplayText] = useState("Friday is listening...")
  const [ttsProvider, setTtsProvider] = useState(() => {
    const saved = localStorage.getItem("karen.ttsProvider")
    return saved === "piper" || saved === "groq" || saved === "auto" ? saved : "auto"
  })
  const [confirmationPending, setConfirmationPending] = useState(false)
  const [gestureEnabled, setGestureEnabled] = useState(() => localStorage.getItem("karen.gestureEnabled") === "true")
  const [llmProvider, setLlmProvider] = useState(() => localStorage.getItem("karen.llmProvider") ?? "groq")
  const [llmModel, setLlmModel] = useState(() => localStorage.getItem("karen.llmModel") ?? (llmProvider === "ollama" ? "qwen2.5:3b" : "openai/gpt-oss-120b"))
  const [missionLog, setMissionLog] = useState<string[]>(["KAREN CORE INITIALIZED", "VOICE LINK STANDING BY"])
  const [lastCommand, setLastCommand] = useState("Awaiting your instruction, Tino.")
  const [lastResponse, setLastResponse] = useState("Friday is online and listening.")
  const [latencyText, setLatencyText] = useState("Awaiting first response")
  const [backgroundTasks, setBackgroundTasks] = useState<BackgroundTask[]>([])

  useEffect(() => {
    void invoke('set_tts_provider', { provider: ttsProvider })
    localStorage.setItem("karen.ttsProvider", ttsProvider)
  }, [ttsProvider])

  useEffect(() => {
    void invoke('set_llm_provider', { provider: llmProvider, model: llmModel })
    localStorage.setItem("karen.llmProvider", llmProvider)
    localStorage.setItem("karen.llmModel", llmModel)
  }, [llmProvider, llmModel])

  useEffect(() => {
    void invoke('set_gesture_enabled', { enabled: gestureEnabled })
    localStorage.setItem("karen.gestureEnabled", String(gestureEnabled))
  }, [gestureEnabled])

  useEffect(() => {
    const unlistenWake = listen('wake-karen', () => {
      // Force wake / barge-in
      setIsMuted(false);
      setOrbState("listening");
      setDisplayText("Hi Tino, I'm listening...");
    });

    const unlistenToggle = listen('toggle-mic', () => {
      setIsMuted((prev) => {
        const nextMuted = !prev;
        if (nextMuted) {
          setOrbState("breathing");
          setDisplayText("Microphone Muted");
        } else {
          setOrbState("listening");
          setDisplayText("Microphone Active");
        }
        return nextMuted;
      });
    });

    const unlistenPushToTalk = listen<PushToTalkEvent>('push-to-talk', ({ payload }) => {
      if (payload.active) {
        setOrbState("listening");
        setDisplayText("Listening… release Ctrl+Alt+M when finished");
      } else {
        setOrbState("working");
        setDisplayText("Transcribing…");
      }
    });

    const unlistenPipeline = listen<PipelineEvent>('karen-pipeline', ({ payload }) => {
      // "answer" fires on every streamed token chunk (the text grows a
      // little each time), not once at the end -- logging each delta as its
      // own entry flooded the mission log with near-duplicate, progressively
      // longer copies of the same response. Every other stage is a discrete,
      // one-off transition and still gets its own line.
      if (payload.stage !== "answer") {
        setMissionLog((items) => [`${new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}  ${payload.detail ?? payload.stage.toUpperCase()}`, ...items].slice(0, 6))
      }
      switch (payload.stage) {
        case "model-missing":
          setOrbState("working");
          setDisplayText(payload.detail ?? "Setting up speech recognition...");
          break;
        case "transcribing":
          setOrbState("working");
          setDisplayText("Transcribing...");
          break;
        case "thinking":
          setOrbState("solving");
          setDisplayText(payload.detail ? `"${payload.detail}"` : "Thinking...");
          if (payload.detail) setLastCommand(payload.detail);
          break;
        case "running-tool":
          setOrbState("connecting");
          setDisplayText(payload.detail ?? "Working on it...");
          break;
        case "confirmation-required":
          setConfirmationPending(true);
          setOrbState("connecting");
          setDisplayText(payload.detail ?? "Confirmation required");
          break;
        case "speaking":
          setOrbState("composing");
          setDisplayText("Friday is speaking…");
          break;
        case "wake-followup":
        case "listening":
          setOrbState("listening");
          setDisplayText(payload.detail ?? "I’m listening…");
          break;
        case "answer":
          setOrbState("breathing");
          setDisplayText(payload.detail ?? "Done");
          if (payload.detail) setLastResponse(payload.detail);
          break;
        case "latency":
          setLatencyText(payload.detail ?? "Latency unavailable");
          break;
        case "error":
          setOrbState("breathing");
          setDisplayText(payload.detail ? `Error: ${payload.detail}` : "Something went wrong");
          break;
        case "idle":
          setOrbState("breathing");
          setDisplayText(payload.detail ?? "Ready");
          break;
      }
    });

    const unlistenTask = listen<TaskEvent>('karen-task', ({ payload }) => {
      setBackgroundTasks((tasks) => {
        const next = tasks.filter((t) => t.id !== payload.task_id);
        next.unshift({ id: payload.task_id, description: payload.description, status: payload.status, result: payload.result });
        return next.slice(0, 5);
      });
    });

    return () => {
      unlistenWake.then(f => f());
      unlistenToggle.then(f => f());
      unlistenPushToTalk.then(f => f());
      unlistenPipeline.then(f => f());
      unlistenTask.then(f => f());
    };
  }, []);

  return (
    <div style={{ minHeight: '100vh', width: '100vw', boxSizing: 'border-box', padding: '28px 34px', color: '#d9faff', fontFamily: 'Inter, system-ui, sans-serif', background: 'radial-gradient(circle at 50% 42%, rgba(0, 171, 214, .16), transparent 28%), linear-gradient(135deg, #03070d, #071521 55%, #02050a)', overflow: 'hidden' }}>
      <header style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', borderBottom: '1px solid rgba(78, 216, 255, .22)', paddingBottom: '14px' }}>
        <div><div style={{ fontSize: '11px', letterSpacing: '4px', color: '#52dfff' }}>FRIDAY // PERSONAL SYSTEM</div><div style={{ fontSize: '25px', letterSpacing: '2px', marginTop: '5px' }}>MISSION CONTROL</div></div>
        <div style={{ textAlign: 'right', fontSize: '11px', color: '#78aebc' }}><div style={{ color: '#72f4ca' }}>● SYSTEMS NOMINAL</div><div>TINO // OPERATOR</div></div>
      </header>

      <main style={{ display: 'grid', gridTemplateColumns: 'minmax(180px, 1fr) minmax(300px, 2fr) minmax(180px, 1fr)', alignItems: 'center', gap: '30px', height: 'calc(100vh - 120px)' }}>
        <section style={{ borderLeft: '2px solid #1599b8', paddingLeft: '16px', color: '#79aebb', fontSize: '12px', lineHeight: '2.2' }}>
          <div style={{ color: '#52dfff', letterSpacing: '2px', marginBottom: '8px' }}>SUBSYSTEMS</div>
          <div><b style={{ color: '#72f4ca' }}>●</b> AUDIO CAPTURE <span style={{ float: 'right' }}>READY</span></div>
          <div><b style={{ color: '#72f4ca' }}>●</b> WHISPER STT <span style={{ float: 'right' }}>READY</span></div>
          <div><b style={{ color: '#72f4ca' }}>●</b> TTS ENGINE <span style={{ float: 'right' }}>READY</span></div>
          <div><b style={{ color: '#52dfff' }}>●</b> MEMORY <span style={{ float: 'right' }}>ONLINE</span></div>
          <div><b style={{ color: '#f6c85f' }}>●</b> LLM LINK <span style={{ float: 'right' }}>{llmProvider.toUpperCase()}</span></div>
        </section>

        <section style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: '22px' }}>
          <div onClick={() => invoke('toggle_mic_command')} style={{ position: 'relative', display: 'grid', placeItems: 'center', width: '270px', height: '270px', borderRadius: '50%', cursor: 'pointer', background: 'radial-gradient(circle, rgba(92, 235, 255, .28) 0 14%, rgba(15, 130, 175, .16) 15% 42%, transparent 43%)', border: `1px solid ${isMuted ? '#ff5577' : '#3fe3ff'}`, boxShadow: `0 0 25px ${isMuted ? '#ff315533' : '#32dfff55'}, inset 0 0 35px #1ba8c622` }}>
            <div style={{ position: 'absolute', inset: '18px', borderRadius: '50%', border: '1px dashed #36cde788', animation: 'spin 18s linear infinite' }} />
            <div style={{ position: 'absolute', inset: '42px', borderRadius: '50%', border: '1px solid #43e7ff66', animation: 'spin 10s linear infinite reverse' }} />
            <div style={{ transform: 'scale(2.2)', opacity: isMuted ? .35 : 1 }}><ThinkingOrb state={orbState} size={64} theme="dark" /></div>
          </div>
          <div style={{ maxWidth: '420px', minHeight: '42px', textAlign: 'center', color: isMuted ? '#ff8395' : '#e7fbff', fontSize: '15px', letterSpacing: '.5px' }}>{displayText}</div>
          <div style={{ color: '#5c9eae', fontSize: '11px', letterSpacing: '2px' }}>CLICK CORE TO {isMuted ? 'ACTIVATE' : 'MUTE'} · CTRL+ALT+M PUSH TO TALK</div>
          <div style={{ color: '#6caebb', fontSize: '10px', letterSpacing: '1px', marginTop: '5px' }}>LATENCY // {latencyText}</div>
        </section>

        <section style={{ borderRight: '2px solid #1599b8', paddingRight: '16px', color: '#79aebb', fontSize: '11px' }}>
          <div style={{ color: '#52dfff', letterSpacing: '2px', marginBottom: '10px' }}>MISSION LOG</div>
          {missionLog.map((item, index) => <div key={`${item}-${index}`} style={{ borderBottom: '1px solid #16404b', padding: '8px 0' }}>{item}</div>)}
        </section>
      </main>

      <section style={{ maxWidth: '760px', margin: '-8px auto 18px', padding: '14px 18px', border: '1px solid #1c6373', borderRadius: '8px', background: 'rgba(3, 18, 28, .78)', boxShadow: '0 0 24px #0bb5d511' }}>
        <div style={{ color: '#52dfff', letterSpacing: '2px', fontSize: '10px', marginBottom: '9px' }}>LIVE DIALOGUE</div>
        <div style={{ color: '#82aeb7', fontSize: '11px', marginBottom: '5px' }}>TINO <span style={{ color: '#426d79' }}>// command</span></div>
        <div style={{ color: '#d7faff', fontSize: '13px', marginBottom: '12px' }}>{lastCommand}</div>
        <div style={{ color: '#72f4ca', fontSize: '11px', marginBottom: '5px' }}>FRIDAY <span style={{ color: '#426d79' }}>// response</span></div>
        <div style={{ color: '#b7dce2', fontSize: '13px', maxHeight: '44px', overflow: 'hidden' }}>{lastResponse}</div>
        <div aria-label="Voice activity" style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', gap: '4px', height: '24px', marginTop: '13px', opacity: isMuted ? .3 : 1 }}>
          {Array.from({ length: 18 }, (_, index) => <span key={index} style={{ width: '3px', height: `${6 + ((index * 7) % 15)}px`, borderRadius: '3px', background: index % 3 === 0 ? '#72f4ca' : '#36d8f5', boxShadow: '0 0 8px #2ad8f577', animation: `wave ${.55 + (index % 5) * .09}s ease-in-out infinite alternate`, animationDelay: `${index * -.06}s` }} />)}
        </div>

        {backgroundTasks.length > 0 && (
          <div style={{ marginTop: '14px', borderTop: '1px solid #16404b', paddingTop: '10px' }}>
            <div style={{ color: '#52dfff', letterSpacing: '2px', fontSize: '10px', marginBottom: '7px' }}>BACKGROUND TOOLS</div>
            {backgroundTasks.map((task) => {
              const badge = {
                queued: { label: 'QUEUED', color: '#f6c85f' },
                running: { label: 'RUNNING', color: '#36d8f5' },
                completed: { label: 'DONE', color: '#72f4ca' },
                failed: { label: 'FAILED', color: '#ff5577' },
              }[task.status];
              return (
                <div key={task.id} style={{ display: 'flex', justifyContent: 'space-between', gap: '10px', fontSize: '11px', padding: '4px 0', color: '#b7dce2' }}>
                  <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{task.description}</span>
                  <span style={{ color: badge.color, letterSpacing: '1px', flexShrink: 0 }}>
                    {task.status === 'running' && <span style={{ display: 'inline-block', marginRight: '5px', width: '6px', height: '6px', borderRadius: '50%', background: badge.color, animation: 'pulse .9s ease-in-out infinite alternate' }} />}
                    {badge.label}
                  </span>
                </div>
              );
            })}
          </div>
        )}
      </section>

      <footer style={{ display: 'flex', justifyContent: 'center', gap: '12px', flexWrap: 'wrap', color: '#83b7c0', fontSize: '12px' }}>
      <button onClick={() => invoke('toggle_mic_command').then(() => setIsMuted((value) => !value))}>{isMuted ? 'Enable mic' : 'Mute mic'}</button>
      <button onClick={() => setGestureEnabled((value) => !value)}>{gestureEnabled ? 'Disable gestures' : 'Enable gestures'}</button>
      <button onClick={() => { setLastResponse('Standing by for your next instruction, Tino.'); setDisplayText('Ready for command.'); }}>Reset status</button>

      <label style={{ color: '#bbb', fontSize: '12px' }}>
        Voice engine{' '}
        <select
          value={ttsProvider}
          onChange={async (event) => {
            const provider = event.target.value;
            setTtsProvider(provider);
          }}
          style={{ marginLeft: '6px', borderRadius: '6px', padding: '3px' }}
        >
          <option value="auto">Groq, then Piper</option>
          <option value="piper">Piper first</option>
          <option value="groq">Groq first</option>
        </select>
      </label>

      <label style={{ color: '#bbb', fontSize: '12px' }}>
        Brain{' '}
        <select value={llmProvider} onChange={(event) => {
          const provider = event.target.value
          setLlmProvider(provider)
          setLlmModel(provider === "ollama" ? "qwen2.5:3b" : "openai/gpt-oss-120b")
        }}>
          <option value="groq">Groq</option>
          <option value="ollama">Ollama VM</option>
        </select>{' '}
        <select value={llmModel} onChange={(event) => setLlmModel(event.target.value)}>
          {llmProvider === "ollama" ? <>
            <option value="qwen2.5:3b">qwen2.5:3b</option>
            <option value="qwen2.5-coder:7b">qwen2.5-coder:7b</option>
          </> : <option value="openai/gpt-oss-120b">gpt-oss-120b</option>}
        </select>
        {llmProvider === "ollama" && <button onClick={async () => {
          try {
            const result = await invoke<{ ok: boolean }>('check_ollama')
            setDisplayText(result.ok ? "Ollama VM is online" : "Ollama VM is unavailable")
          } catch {
            setDisplayText("Ollama VM is unavailable")
          }
        }}>Check</button>}
      </label>

      {confirmationPending && (
        <div style={{ display: 'flex', gap: '8px' }}>
          <button onClick={async () => {
            await invoke('confirm_pending_tool');
            setConfirmationPending(false);
          }}>Approve</button>
          <button onClick={async () => {
            await invoke('cancel_pending_tool');
            setConfirmationPending(false);
            setDisplayText("Action cancelled");
          }}>Cancel</button>
        </div>
      )}
      </footer>
      <style>{`@keyframes spin { from { transform: rotate(0deg) } to { transform: rotate(360deg) } } @keyframes wave { from { transform: scaleY(.45); opacity:.45 } to { transform: scaleY(1.25); opacity:1 } } @keyframes pulse { from { opacity: .3; transform: scale(.8) } to { opacity: 1; transform: scale(1.15) } } select,button { background:#091c28; color:#c8f7ff; border:1px solid #286477; border-radius:5px; padding:5px 8px } button { cursor:pointer }`}</style>
    </div>
  )
}

export default App
