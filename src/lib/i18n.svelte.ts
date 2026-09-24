export type Lang = "pt" | "en";

const en = {
  "title.settings": "Synapse - Settings",
  "title.history": "Synapse - History",
  "app.settings": "Settings",
  "app.history": "History",
  "win.minimize": "Minimize",
  "win.close": "Close",

  "common.loading": "Loading…",
  "common.cancel": "Cancel",
  "common.retry": "Retry",
  "common.delete": "Delete",
  "common.failed": "Failed",
  "common.done": "Done",

  "save.saved": "Saved",
  "save.failed": "Couldn't save",

  "load.slow": "Still starting up…",
  "load.slowSub": "It will load on its own.",

  "nav.general": "General",
  "nav.voice": "Voice",
  "nav.ai": "AI",
  "nav.dictionary": "Dictionary",
  "nav.advanced": "Advanced",

  "gen.shortcut": "Shortcut",
  "gen.change": "Change",
  "gen.pressKeys": "Press the keys…",
  "gen.pressKeysMouse": "Press keys or a mouse button…",
  "gen.none": "None",
  "gen.mode": "Mode",
  "gen.modeHold": "Hold",
  "gen.modeTap": "Tap",
  "gen.speechLang": "Speech language",
  "gen.mic": "Microphone",
  "gen.micDefault": "System default",
  "gen.startWin": "Start with Windows",
  "gen.startMac": "Start with macOS",
  "gen.disabledByWindows": "Disabled in Task Manager",
  "gen.autostartFailed": "Couldn't change startup",
  "gen.theme": "Theme",
  "gen.themeLight": "Light",
  "gen.themeSystem": "System",
  "gen.themeDark": "Dark",
  "gen.appLang": "App language",
  "gen.appLangAuto": "Automatic",
  "lang.pt": "Português",
  "lang.en": "English",

  "key.mouseMiddle": "Mouse 3 (middle)",
  "key.mouseBack": "Mouse 4 (side)",
  "key.mouseForward": "Mouse 5 (side)",
  "key.space": "Space",
  "key.up": "Up",
  "key.down": "Down",
  "key.left": "Left",
  "key.right": "Right",

  "sl.auto": "Auto-detect",
  "sl.pt": "Portuguese",
  "sl.en": "English",
  "sl.es": "Spanish",
  "sl.fr": "French",
  "sl.de": "German",
  "sl.it": "Italian",
  "sl.ja": "Japanese",
  "sl.zh": "Chinese",
  "sl.ru": "Russian",
  "sl.ko": "Korean",

  "tt.none": "Don't translate",
  "tt.English": "English",
  "tt.Spanish": "Spanish",
  "tt.Brazilian Portuguese": "Portuguese (BR)",
  "tt.French": "French",
  "tt.German": "German",
  "tt.Italian": "Italian",
  "tt.Japanese": "Japanese",
  "tt.Simplified Chinese": "Chinese (Simplified)",
  "tt.Russian": "Russian",
  "tt.Korean": "Korean",

  "voice.engine": "Transcription",
  "voice.onPc": "On this PC",
  "voice.groq": "Groq",
  "voice.slow": "slow on this PC",
  "voice.groqKey": "Groq key",
  "voice.getKey": "Get key",
  "voice.model": "Model",
  "voice.groqTurbo": "turbo",
  "voice.groqLarge": "large-v3",
  "voice.fillers": "Remove hesitations",

  "secret.show": "Show key",
  "secret.hide": "Hide key",

  "model.recommended": "recommended",
  "model.active": "Active",
  "model.use": "Use",
  "model.download": "Download",
  "model.starting": "Starting…",
  "model.failed": "Failed",
  "model.deleteLabel": "Delete model",
  "model.downloadFailed": "Download failed",
  "model.activateFailed": "Couldn't activate",
  "model.deleteFailed": "Couldn't delete",
  "model.empty": "No models",

  "ai.correct": "Correct text",
  "ai.translate": "Translate to",
  "ai.paragraphs": "Organize into paragraphs",
  "ai.provider": "Provider",
  "ai.local": "Local",
  "ai.groq": "Groq",
  "ai.other": "Other",
  "ai.install": "Install",
  "ai.installFailed": "Installation failed",
  "ai.groqKey": "Groq key (AI)",
  "ai.model": "Model",
  "ai.type": "Type",
  "ai.typeOpenai": "OpenAI-compatible",
  "ai.typeAnthropic": "Anthropic",
  "ai.typeOllama": "Ollama",
  "ai.endpoint": "Address",
  "ai.modelName": "Model",
  "ai.key": "Key",
  "ai.test": "Test",
  "ai.testing": "Testing…",
  "ai.testOk": "Working",

  "stage.resolve_release": "Preparing…",
  "stage.download_binary": "Downloading server…",
  "stage.unzip": "Extracting…",
  "stage.download_model": "Downloading model…",
  "stage.configure_start": "Starting…",

  "dict.spoken": "As spoken",
  "dict.replacement": "Replace with",
  "dict.add": "Add",
  "dict.remove": "Remove",
  "dict.empty": "No replacements yet",
  "dict.vocab": "Vocabulary",
  "dict.vocabHint": "One word per line",

  "adv.audio": "Audio",
  "adv.vad": "Voice detection (VAD)",
  "adv.sensitivity": "Sensitivity",
  "adv.padding": "Padding",
  "adv.minSilence": "Minimum silence",
  "adv.paste": "Paste",
  "adv.restoreClipboard": "Restore clipboard",
  "adv.pasteDelay": "Paste delay",
  "adv.performance": "Performance",
  "adv.gpu": "Use GPU",
  "adv.ai": "AI",
  "adv.timeout": "Timeout",
  "adv.temperature": "Temperature",
  "adv.testConn": "Test connection",
  "adv.restartAi": "Restart local AI",
  "adv.restarting": "Restarting…",
  "adv.fillers": "Filler words",
  "adv.fillersHint": "One per line",
  "adv.extraModels": "Extra models",

  "hf.placeholder": "Hugging Face link",
  "hf.detect": "Detect",
  "hf.invalid": "Invalid link",
  "hf.failed": "Couldn't read the link",
  "hf.noFiles": "No model files found",
  "hf.kindVoice": "Voice",
  "hf.kindAi": "AI",
  "hf.add": "Add and download",
  "hf.addFailed": "Couldn't add",
  "hf.addedVoice": "Added to Voice",
  "hf.addedAi": "Added to AI",

  "err.groq_key_missing": "Add your Groq key",
  "err.groq_llm_key_missing": "Add the Groq AI key",
  "err.mic_unavailable": "Microphone unavailable",
  "err.model_missing": "Download the voice model",
  "err.engine_loading": "Model still loading",
  "err.engine_error": "Model error, retrying",
  "err.transcription_failed": "Transcription failed",
  "err.llm_not_ready": "AI starting, pasted as is",
  "err.llm_not_configured": "Set up the AI to translate",
  "err.llm_failed": "AI failed, pasted as is",
  "err.llm_timeout": "AI timed out, pasted as is",
  "err.hotkey_failed": "Shortcut unavailable, retrying",
  "err.busy": "Still processing",
  "err.settings_unreadable": "Settings locked, using defaults",
  "err.max_duration": "Maximum duration reached",
  "err.inject_failed": "Couldn't paste, text is in History",

  "w.starting": "Starting…",
  "w.noMic": "No microphone",
  "w.loading": "Loading…",
  "w.downloadModel": "Download the model",
  "w.modelError": "Model error",
  "w.hotkeyUnavailable": "Shortcut unavailable",
  "w.cancelling": "Cancelling…",
  "w.takingLong": "Taking long {s}s",
  "w.transcribingS": "Transcribing {s}s",
  "w.transcribing": "Transcribing…",
  "w.noSpeech": "No speech detected",
  "w.cancelled": "Cancelled",
  "w.settings": "Settings",
  "w.history": "History",
  "w.stop": "Stop recording",
  "w.cancelTx": "Cancel transcription",
  "w.start": "Start recording",
  "w.level": "Microphone level",

  "h.clearAsk": "Clear all history?",
  "h.confirm": "Confirm",
  "h.clearAll": "Clear all",
  "h.words": "Words",
  "h.speaking": "Speaking time",
  "h.avgWpm": "Avg WPM",
  "h.wpm": "WPM",
  "h.wpmTitle": "Words per minute",
  "h.entries": "Entries",
  "h.search": "Search history…",
  "h.entryOne": "entry",
  "h.entryMany": "entries",
  "h.raw": "raw",
  "h.cloud": "Cloud",
  "h.gpu": "GPU",
  "h.cpu": "CPU",
  "h.ai": "AI",
  "h.copy": "Copy",
  "h.copied": "Copied",
  "h.dictionary": "Dictionary",
  "h.delete": "Delete",
  "h.empty": "No transcriptions yet. Use the shortcut and speak.",
  "h.promptPhrase": "Spoken phrase (as Whisper heard it):",
  "h.promptReplacement": "Exact replacement:",
  "h.sec": "s",
  "h.min": "min",
  "h.hour": "h",
};

export type TKey = keyof typeof en;

const pt: Record<TKey, string> = {
  "title.settings": "Synapse - Ajustes",
  "title.history": "Synapse - Histórico",
  "app.settings": "Ajustes",
  "app.history": "Histórico",
  "win.minimize": "Minimizar",
  "win.close": "Fechar",

  "common.loading": "Carregando…",
  "common.cancel": "Cancelar",
  "common.retry": "Tentar de novo",
  "common.delete": "Excluir",
  "common.failed": "Falhou",
  "common.done": "Pronto",

  "save.saved": "Salvo",
  "save.failed": "Não foi possível salvar",

  "load.slow": "Ainda iniciando…",
  "load.slowSub": "Vai carregar sozinho.",

  "nav.general": "Geral",
  "nav.voice": "Voz",
  "nav.ai": "IA",
  "nav.dictionary": "Dicionário",
  "nav.advanced": "Avançado",

  "gen.shortcut": "Atalho",
  "gen.change": "Alterar",
  "gen.pressKeys": "Pressione as teclas…",
  "gen.pressKeysMouse": "Pressione teclas ou botão do mouse…",
  "gen.none": "Nenhum",
  "gen.mode": "Modo",
  "gen.modeHold": "Segurar",
  "gen.modeTap": "Tocar",
  "gen.speechLang": "Idioma da fala",
  "gen.mic": "Microfone",
  "gen.micDefault": "Padrão do sistema",
  "gen.startWin": "Iniciar com o Windows",
  "gen.startMac": "Iniciar com o macOS",
  "gen.disabledByWindows": "Desativado no Gerenciador de Tarefas",
  "gen.autostartFailed": "Não foi possível alterar",
  "gen.theme": "Tema",
  "gen.themeLight": "Claro",
  "gen.themeSystem": "Sistema",
  "gen.themeDark": "Escuro",
  "gen.appLang": "Idioma do app",
  "gen.appLangAuto": "Automático",
  "lang.pt": "Português",
  "lang.en": "English",

  "key.mouseMiddle": "Mouse 3 (meio)",
  "key.mouseBack": "Mouse 4 (lateral)",
  "key.mouseForward": "Mouse 5 (lateral)",
  "key.space": "Espaço",
  "key.up": "Cima",
  "key.down": "Baixo",
  "key.left": "Esquerda",
  "key.right": "Direita",

  "sl.auto": "Detectar automaticamente",
  "sl.pt": "Português",
  "sl.en": "Inglês",
  "sl.es": "Espanhol",
  "sl.fr": "Francês",
  "sl.de": "Alemão",
  "sl.it": "Italiano",
  "sl.ja": "Japonês",
  "sl.zh": "Chinês",
  "sl.ru": "Russo",
  "sl.ko": "Coreano",

  "tt.none": "Não traduzir",
  "tt.English": "Inglês",
  "tt.Spanish": "Espanhol",
  "tt.Brazilian Portuguese": "Português (BR)",
  "tt.French": "Francês",
  "tt.German": "Alemão",
  "tt.Italian": "Italiano",
  "tt.Japanese": "Japonês",
  "tt.Simplified Chinese": "Chinês (simplificado)",
  "tt.Russian": "Russo",
  "tt.Korean": "Coreano",

  "voice.engine": "Transcrição",
  "voice.onPc": "No PC",
  "voice.groq": "Groq",
  "voice.slow": "lento neste PC",
  "voice.groqKey": "Chave do Groq",
  "voice.getKey": "Obter chave",
  "voice.model": "Modelo",
  "voice.groqTurbo": "turbo",
  "voice.groqLarge": "large-v3",
  "voice.fillers": "Remover hesitações",

  "secret.show": "Mostrar chave",
  "secret.hide": "Ocultar chave",

  "model.recommended": "recomendado",
  "model.active": "Ativo",
  "model.use": "Usar",
  "model.download": "Baixar",
  "model.starting": "Iniciando…",
  "model.failed": "Falhou",
  "model.deleteLabel": "Excluir modelo",
  "model.downloadFailed": "Falha no download",
  "model.activateFailed": "Não foi possível ativar",
  "model.deleteFailed": "Não foi possível excluir",
  "model.empty": "Nenhum modelo",

  "ai.correct": "Corrigir texto",
  "ai.translate": "Traduzir para",
  "ai.paragraphs": "Organizar em parágrafos",
  "ai.provider": "Provedor",
  "ai.local": "Local",
  "ai.groq": "Groq",
  "ai.other": "Outro",
  "ai.install": "Instalar",
  "ai.installFailed": "Falha na instalação",
  "ai.groqKey": "Chave do Groq (IA)",
  "ai.model": "Modelo",
  "ai.type": "Tipo",
  "ai.typeOpenai": "OpenAI-compatível",
  "ai.typeAnthropic": "Anthropic",
  "ai.typeOllama": "Ollama",
  "ai.endpoint": "Endereço",
  "ai.modelName": "Modelo",
  "ai.key": "Chave",
  "ai.test": "Testar",
  "ai.testing": "Testando…",
  "ai.testOk": "Funcionando",

  "stage.resolve_release": "Preparando…",
  "stage.download_binary": "Baixando servidor…",
  "stage.unzip": "Extraindo…",
  "stage.download_model": "Baixando modelo…",
  "stage.configure_start": "Iniciando…",

  "dict.spoken": "Como você fala",
  "dict.replacement": "Substituir por",
  "dict.add": "Adicionar",
  "dict.remove": "Remover",
  "dict.empty": "Nenhuma substituição",
  "dict.vocab": "Vocabulário",
  "dict.vocabHint": "Uma palavra por linha",

  "adv.audio": "Áudio",
  "adv.vad": "Detecção de voz (VAD)",
  "adv.sensitivity": "Sensibilidade",
  "adv.padding": "Margem",
  "adv.minSilence": "Silêncio mínimo",
  "adv.paste": "Colar",
  "adv.restoreClipboard": "Restaurar área de transferência",
  "adv.pasteDelay": "Atraso ao colar",
  "adv.performance": "Desempenho",
  "adv.gpu": "Usar GPU",
  "adv.ai": "IA",
  "adv.timeout": "Tempo limite",
  "adv.temperature": "Temperatura",
  "adv.testConn": "Testar conexão",
  "adv.restartAi": "Reiniciar IA local",
  "adv.restarting": "Reiniciando…",
  "adv.fillers": "Palavras de hesitação",
  "adv.fillersHint": "Uma por linha",
  "adv.extraModels": "Modelos extras",

  "hf.placeholder": "Link do Hugging Face",
  "hf.detect": "Detectar",
  "hf.invalid": "Link inválido",
  "hf.failed": "Não foi possível ler o link",
  "hf.noFiles": "Nenhum arquivo de modelo",
  "hf.kindVoice": "Voz",
  "hf.kindAi": "IA",
  "hf.add": "Adicionar e baixar",
  "hf.addFailed": "Não foi possível adicionar",
  "hf.addedVoice": "Adicionado em Voz",
  "hf.addedAi": "Adicionado em IA",

  "err.groq_key_missing": "Adicione a chave do Groq",
  "err.groq_llm_key_missing": "Adicione a chave do Groq (IA)",
  "err.mic_unavailable": "Microfone indisponível",
  "err.model_missing": "Baixe o modelo de voz",
  "err.engine_loading": "Modelo ainda carregando",
  "err.engine_error": "Erro no modelo, tentando de novo",
  "err.transcription_failed": "Falha na transcrição",
  "err.llm_not_ready": "IA iniciando, colado sem correção",
  "err.llm_not_configured": "Configure a IA para traduzir",
  "err.llm_failed": "Falha na IA, colado sem correção",
  "err.llm_timeout": "IA demorou, colado sem correção",
  "err.hotkey_failed": "Atalho indisponível, tentando de novo",
  "err.busy": "Ainda processando",
  "err.settings_unreadable": "Ajustes bloqueados, usando o padrão",
  "err.max_duration": "Duração máxima atingida",
  "err.inject_failed": "Não foi possível colar, o texto está no Histórico",

  "w.starting": "Iniciando…",
  "w.noMic": "Sem microfone",
  "w.loading": "Carregando…",
  "w.downloadModel": "Baixe o modelo",
  "w.modelError": "Erro no modelo",
  "w.hotkeyUnavailable": "Atalho indisponível",
  "w.cancelling": "Cancelando…",
  "w.takingLong": "Demorando {s}s",
  "w.transcribingS": "Transcrevendo {s}s",
  "w.transcribing": "Transcrevendo…",
  "w.noSpeech": "Nenhuma fala detectada",
  "w.cancelled": "Cancelado",
  "w.settings": "Ajustes",
  "w.history": "Histórico",
  "w.stop": "Parar gravação",
  "w.cancelTx": "Cancelar transcrição",
  "w.start": "Iniciar gravação",
  "w.level": "Nível do microfone",

  "h.clearAsk": "Limpar todo o histórico?",
  "h.confirm": "Confirmar",
  "h.clearAll": "Limpar tudo",
  "h.words": "Palavras",
  "h.speaking": "Tempo falando",
  "h.avgWpm": "Média PPM",
  "h.wpm": "PPM",
  "h.wpmTitle": "Palavras por minuto",
  "h.entries": "Registros",
  "h.search": "Buscar no histórico…",
  "h.entryOne": "registro",
  "h.entryMany": "registros",
  "h.raw": "original",
  "h.cloud": "Nuvem",
  "h.gpu": "GPU",
  "h.cpu": "CPU",
  "h.ai": "IA",
  "h.copy": "Copiar",
  "h.copied": "Copiado",
  "h.dictionary": "Dicionário",
  "h.delete": "Excluir",
  "h.empty": "Nenhuma transcrição ainda. Use o atalho e fale.",
  "h.promptPhrase": "Frase falada (como o Whisper ouviu):",
  "h.promptReplacement": "Substituição exata:",
  "h.sec": "s",
  "h.min": "min",
  "h.hour": "h",
};

const DICTS: Record<Lang, Record<string, string>> = { en, pt };

function systemLang(): Lang {
  try {
    const nav = typeof navigator !== "undefined" ? navigator.language : "";
    return (nav || "").toLowerCase().startsWith("pt") ? "pt" : "en";
  } catch {
    return "en";
  }
}

export function resolveLang(pref: unknown): Lang {
  if (pref === "pt" || pref === "en") return pref;
  return systemLang();
}

const current = $state<{ lang: Lang }>({ lang: resolveLang("auto") });

function applyDocumentLang(lang: Lang) {
  try {
    document.documentElement.lang = lang === "pt" ? "pt-BR" : "en";
  } catch {
    return;
  }
}

applyDocumentLang(current.lang);

export function setLanguage(pref: unknown) {
  const next = resolveLang(pref);
  if (current.lang !== next) current.lang = next;
  applyDocumentLang(next);
}

export function lang(): Lang {
  return current.lang;
}

export function locale(): string {
  return current.lang === "pt" ? "pt-BR" : "en-US";
}

type Params = Record<string, string | number>;

function own(dict: Record<string, string>, key: string): string | undefined {
  return Object.prototype.hasOwnProperty.call(dict, key) ? dict[key] : undefined;
}

function format(text: string, params?: Params): string {
  if (!params) return text;
  return text.replace(/\{(\w+)\}/g, (match, name: string) =>
    Object.prototype.hasOwnProperty.call(params, name) ? String(params[name]) : match,
  );
}

const warned = new Set<string>();

export function t(key: TKey, params?: Params): string {
  const active = current.lang;
  const hit = own(DICTS[active], key);
  if (hit !== undefined) return format(hit, params);
  if (import.meta.env.DEV && !warned.has(`${active}:${key}`)) {
    warned.add(`${active}:${key}`);
    console.warn(`i18n missing key ${active}.${key}`);
  }
  return format(own(DICTS.en, key) ?? key, params);
}

export function tOr(key: string, fallback: string, params?: Params): string {
  const active = current.lang;
  const hit = own(DICTS[active], key) ?? own(DICTS.en, key);
  return hit !== undefined ? format(hit, params) : fallback;
}
