// Every string Hotend's screens show (DESIGN_SPEC 11), in one catalogue so that
// translation is possible later. Sentences of the engine and the core are shown word for
// word and are not here.
import { count, duration } from "./format";

const problems = (n: number) => count(n, "problem", "problems");
const notes = (n: number) => count(n, "note", "notes");

export const S = {
  app: "Hotend",
  printerTag: "Simulated",
  home: {
    question: "What do you want to make?",
    placeholder: "Describe it, with the sizes you know",
    send: "Start designing",
    projects: "Projects",
    empty: "Your projects will appear here.",
    exampleTag: "Example",
    exampleTitle: "Cable box with a snap-on lid",
    exampleLine: "Open it to look around",
    // What is true of the example as it opens.
    exampleUnder: "Two parts · 3 sizes to measure",
    exampleName: "Cable box (example)",
    exampleNote: "The bundled example: a cable box with a snap-on lid",
    exampleBuilding: "Building the example…",
    noKey: "Add an AI key to start.",
    search: "Search projects",
    noMatch: "No project has that name.",
    folderFailed: "Hotend could not open its project folder.",
    tryAgain: "Try again",
    healthFailed: "Open This computer",
    open: (name: string) => `Open ${name}`,
  },
  suggestions: [
    "A box that hides a power strip, with a snap-on lid",
    // A number and its unit stay on one line.
    "A wall hook for a 32\u00a0mm pipe, held by two screws",
    "A drawer divider for cutlery, 300\u00a0mm long",
  ],
  card: {
    nothing: "Nothing built yet",
    problems: (n: number) => problems(n),
    toMeasure: (n: number) => `${n} to measure`,
    notes: (n: number) => `Ready, ${notes(n)}`,
    ready: "Ready",
    printing: (percent: number | null) => (percent === null ? "Printing" : `Printing ${percent}\u00a0%`),
    simulatedPrint: (percent: number | null) => (percent === null ? "Simulated print" : `Simulated print · ${percent}\u00a0%`),
    working: "Hotend is working…",
  },
  stages: { design: "Design", prepare: "Prepare", print: "Print", switch: "Stage" },
  marks: {
    busy: "working",
    problem: (n: number) => problems(n),
    problemPlain: "a problem",
    measure: "numbers to measure",
    attention: "needs you",
    done: "done",
    sim: "simulated, nothing made",
  },
  studio: {
    gone: "This project is not here any more.",
  },
  error: { details: "Details" },
  titlebar: {
    home: "Home",
    conversation: "Conversation",
    inspector: "Numbers",
    settings: "Settings",
    projectMenu: "Project",
    newText: "Hotend wrote something",
  },
  project: {
    export: "Export files…",
    exportTitle: "Export files",
    exportGo: "Choose a folder and export…",
    exportClose: "Close",
    exporting: "Exporting…",
    exported: (n: number) => `${count(n, "file", "files")} saved.`,
    exportFolderTitle: "Choose a folder for the exported files",
    formats: { step: "STEP", "3mf": "3MF", stl: "STL" } as Record<string, string>,
    formatNotes: {
      step: "exact shape, for other CAD programs",
      "3mf": "all parts, for a slicer",
      stl: "one mesh per part",
    } as Record<string, string>,
    folder: "Files",
    copyPath: "Copy path",
    copied: "Copied",
  },
  stage: {
    empty: "Nothing here yet. Describe what you want to make.",
    emptyHint: "The part appears here as soon as Hotend has built it.",
    plate: (name: string, x: number, y: number) => `${name} · plate ${x} × ${y} mm · Simulated`,
    plateShort: (x: number, y: number) => `Plate ${x} × ${y} mm · X2D Simulated`,
    fit: "Fit",
    views: "Views",
    cut: "Cut open",
    cutTitle: "Cut open",
    cutPlanes: { x: "Across", y: "Along", z: "From the top" },
    cutSlider: "Where the cut is",
    viewNames: { iso: "Corner", front: "Front", top: "Top", right: "Right" },
    solidHint: (grams: string) => `About ${grams} if it were printed completely full. Prepare gives the real weight.`,
    partShown: "Shown. Click to hide.",
    partHidden: "Hidden. Click to show.",
    hide: (part: string) => `Hide ${part}`,
    show: (part: string) => `Show ${part}`,
    view3d: (project: string, parts: string, size: string) => `3D view of ${project}: ${parts}, ${size}`,
    view3dEmpty: (project: string) => `3D view of ${project}: nothing built yet`,
    noView: "This version has no 3D view.",
    noWebgl: "This computer cannot show 3D here.",
    openComputer: "Open This computer",
    picked: (part: string, place: string) => `${part} · ${place}`,
    pickedAt: (where: string) => `Near ${where}`,
    places: {
      face: {
        top: "the top",
        bottom: "the bottom",
        long: "a long side",
        short: "a short side",
        side: "a side",
        inside: "an inside face",
      },
      edge: {
        top: "an edge at the top",
        bottom: "an edge at the bottom",
        long: "an edge of a long side",
        short: "an edge of a short side",
        side: "an upright edge",
        inside: "an inside edge",
      },
      vertex: {
        top: "a corner at the top",
        bottom: "a corner at the bottom",
        long: "a corner on a long side",
        short: "a corner on a short side",
        side: "a corner",
        inside: "an inside corner",
      },
      unknown: { face: "a face", edge: "an edge", vertex: "a corner" },
    },
    mention: "Mention in message",
    mentionText: (part: string, place: string, where: string | null) =>
      where === null ? `On the ${part}, at ${place}: ` : `On the ${part}, at ${place} (near ${where}): `,
    clearPick: "Clear",
    printCaption: (x: number, y: number) => `Plate ${x} × ${y} mm · X2D Simulated · drawn from the layer count`,
    printCaptionHint: "The picture is drawn from the printer's layer count. There is no camera in this version.",
    stopped: "Hotend was stopped. Send your description again to build it.",
    moreParts: (n: number) => (n === 1 ? "+1 part" : `+${n} parts`),
    showAllParts: "Show all",
    simBand: "Simulated printer: nothing is being printed",
  },
  verdict: {
    noKey: "Add an AI key to start designing.",
    openSettings: "Open Settings",
    openExample: "Open the example",
    working: (line: string) => `Hotend is working: ${line}`,
    stop: "Stop",
    stopping: "Stopping…",
    rebuilding: (label: string, value: string) => `Rebuilding with ${label} ${value}`,
    rebuildingAgain: "Rebuilding with your printer's measurements",
    slow: (doing: string) => `Taking longer than usual: ${doing.charAt(0).toLowerCase()}${doing.slice(1)}`,
    rebuildFailed: (still: string) => `Could not rebuild. Still at ${still}.`,
    looking: (n: number, of: number) => `Looking at v${n} of ${of}`,
    backTo: (n: number) => `Back to v${n}`,
    use: (n: number) => `Use v${n}`,
    refused: (label: string, value: string, still: string) => `${label} ${value} does not build. Still at ${still}.`,
    sizeProblem: (label: string, value: string, n: number, title: string) =>
      n === 1 ? `${label} ${value} gives 1 problem: ${title}` : `${label} ${value} gives ${problems(n)}`,
    undo: "Undo that change",
    ok: "OK",
    stoppedEmpty: "Stopped before anything was built.",
    sendItAgain: "Send it again",
    askFix: "Ask Hotend to fix it",
    showMe: "Show me",
    showAll: "Show all parts",
    problem: (n: number, title: string) => (n === 1 ? `1 problem: ${title}` : `${problems(n)} to fix`),
    guesses: (n: number) => (n === 1 ? "1 number is a guess" : `${n} numbers are guesses`),
    measure: (n: number) => (n === 1 ? "Measure it" : "Measure them"),
    prepareAnyway: "Prepare anyway",
    readyNotes: (n: number) => `Ready to print, with ${notes(n)}`,
    readIt: "Read it",
    ready: "Ready to print",
    prepare: "Prepare",
    // Prepare
    nothingToPrepare: "Nothing to prepare yet.",
    nothingToPrint: "Nothing to print yet.",
    goDesign: "Go to Design",
    fixFirst: (n: number) => `Fix ${problems(n)} in Design first.`,
    layingOut: "Laying out the plate",
    doesNotFit: (name: string) => `The ${name} does not fit on a plate. Turn it, or make it smaller.`,
    slicing: (plate: number) => `Slicing plate ${plate}`,
    noSlicer: "No slicer is set up, so there is no time or weight yet.",
    sliceAgain: "Slice again",
    changed: "Changed since the last slice.",
    previewsOutdated: "Sliced before Hotend drew plate previews.",
    fitsOne: "Fits on one plate.",
    fitsMany: (n: number, showing: number) => `Fits on ${n} plates. Showing plate ${showing}.`,
    slice: "Slice",
    sliced: (time: string, grams: string, rough: boolean) =>
      rough ? `Sliced (rough): about ${time} · about ${grams}` : `Sliced: ${time} · ${grams}`,
    slicedPlate: (plate: number, of: number, time: string, grams: string, rough: boolean) =>
      rough
        ? `Plate ${plate} of ${of} sliced (rough): about ${time} · about ${grams}`
        : `Plate ${plate} of ${of} sliced: ${time} · ${grams}`,
    review: "Review print",
    reviewPlate: (plate: number) => `Review plate ${plate}`,
    // Print
    sliceFirst: "Slice this version first.",
    goPrepare: "Go to Prepare",
    waitingDialog: "Waiting for your answer in the Hotend dialog",
    waitingStart: (clock: string) => `Waiting for the printer to start · ${clock}`,
    gettingReady: (what: string) => `Getting ready: ${what}`,
    printing: (layer: number | null, total: number | null, percent: number | null) =>
      layer !== null && total !== null
        ? `Printing · layer ${layer} of ${total}`
        : percent !== null
          ? `Printing · ${percent} %`
          : "Printing",
    finished: "Finished.",
    finishedSim: "Simulated print finished. Nothing was made.",
    finishedPlate: (plate: number, of: number) => `Plate ${plate} of ${of} finished.`,
    goPlate: (plate: number) => `Go to plate ${plate}`,
    backDesign: "Back to Design",
    runAgain: "Run it again",
    cancelledSim: "Simulated print stopped. Nothing was made.",
    printAgain: "Print again",
    reviewAgain: "Review again",
    notStarted: "The printer accepted the job but did not start.",
    failed: "The printer stopped.",
    cancelled: "Stopped. Nothing more will be printed.",
    saved: "Saved to the folder.",
    // Hotend saves the file; Bambu Connect was asked to import it only when `opened` says
    // so (the core's record). Either way the person chooses the printer and presses Print.
    handedOff: (opened: boolean) =>
      opened
        ? "Saved, and Bambu Connect was asked to import it. Choose the printer and press Print there."
        : "Saved for Bambu Connect. Import it there, choose the printer and press Print.",
    cannotSend: "This print cannot be sent yet.",
    backPrepare: "Back to Prepare",
    checking: "Checking the sliced file",
    checkNumbers: "Check the numbers, then start",
    offline: "Lost contact with the printer.",
  },
  talk: {
    you: "You",
    hotend: "Hotend",
    placeholderEmpty: "Describe what you want to make",
    placeholder: "Ask for a change…",
    working: "Hotend is working",
    send: "Send",
    stop: "Stop",
    olderVersion: (n: number) => `Messages change v${n}, the current version.`,
    noKey: "Add an AI key to talk to Hotend.",
    otherProvider: (provider: string) =>
      `This project was started with ${provider}. Switch back to it in Settings to continue the conversation.`,
    notRemembered: "Hotend will not remember this part of the conversation.",
    stoppedAt: (n: number) => `Stopped. v${n} is still here.`,
    stoppedNothing: "Stopped. Nothing was made.",
    // An answer in words alone took no steps and says so.
    noSteps: "Answered without changing the part",
    tokens: (tokens: string) => `${tokens} tokens used`,
    showSteps: (n: number) => `Show steps (${n})`,
    hideSteps: (n: number) => `Hide steps (${n})`,
    proposed: "Hotend prepared a print for you to review.",
    goPrint: "Go to Print",
    sending: "Sending…",
    // Next to an answer that would have looked something up with web research off.
    research: {
      turnOn: "Turn on web research",
      why: (lookup: string) => (lookup ? `Hotend would look up: ${lookup}` : "Hotend would look this up on the web."),
      on: "Web research is on. Send your message again and Hotend will look it up.",
      declined: "Web research stays off.",
      chooseSource: "Choose a web research source",
      settings: "Web research settings",
      // Why the run had no research, for each state of the core but "not agreed yet".
      reasons: {
        noSource: "No web research source is chosen.",
        off: "Web research is off for this project.",
        needs_key: "The search service needs an API key.",
        provider_refused: "The AI provider refused web search for this account.",
        unsupported_provider: "This AI provider has no web research in Hotend.",
        budget: "This project has used its web research allowance.",
      },
    },
  },
  work: {
    thinking: "Thinking…",
    writing: (note: string | null) => (note ? `Writing the model: ${note}` : "Writing the model…"),
    changing: (label: string | null) => (label ? `Changing ${label}…` : "Changing a size…"),
    checking: "Checking it…",
    looking: "Looking at it…",
    measuring: "Measuring…",
    reading: "Reading the manual…",
    slicing: "Slicing…",
    proposing: "Getting a print ready for you to review…",
    other: "Working…",
  },
  steps: {
    wrote: (version: number | null) => (version === null ? "Wrote the model" : `Wrote the model · v${version}`),
    wroteFailed: "The model did not build, trying again",
    set: (label: string, value: string, version: number) => `Set ${label} to ${value} · v${version}`,
    setPlain: "Changed a size",
    setFailed: "That size does not build",
    checked: "Checked it",
    looked: "Looked at it",
    measured: "Measured it",
    sliced: (time: string, grams: string) => `Sliced: ${time}, ${grams}`,
    slicedPlain: "Sliced it",
    sliceFailed: "Could not slice",
    proposed: "Prepared a print for you to review",
    proposeFailed: "Could not prepare a print",
    read: "Read the manual",
    other: "Did something else",
  },
  marker: {
    written: "Hotend",
    sizeSet: (label: string, value: string) => `${label} set to ${value}`,
    sizeChanged: "A size changed",
    builtAgain: "Built again with your printer's values",
    sizeChange: (label: string, from: string, to: string) => `${label} ${from} → ${to}`,
    nowPasses: (titles: string) => `Now passes: ${titles}`,
    nowFails: (titles: string) => `Now fails: ${titles}`,
    same: "Same sizes, same proof.",
    look: (n: number) => `Look at v${n}`,
  },
  runError: {
    noKey: (provider: string) =>
      `There is no AI key for ${provider}. Add one, then send the message again. Nothing was lost.`,
    busy: "Hotend is still working on the last message.",
    stopIt: "Stop it",
    budget: (steps: number, version: number | null) =>
      version === null
        ? `Hotend used all ${steps} steps for this message and stopped.`
        : `Hotend used all ${steps} steps for this message and stopped at v${version}.`,
    continue: "Continue",
    continueText: "Continue.",
    limit: (version: number | null) =>
      version === null
        ? "This message reached Hotend's limit for one run."
        : `This message reached Hotend's limit for one run. The part is at v${version}.`,
    refusals: "Hotend stopped this run because the AI kept asking for things it is not allowed to do.",
    tryAgain: "Try again",
    sendAgain: "Send again",
    sandbox: "Hotend cannot run AI-written models safely on this computer, so it did not run this one.",
    openComputer: "Open This computer",
    other: "Hotend could not finish this message.",
    unchanged: "Your project is unchanged.",
    partAt: (version: number) => `The part is at v${version}.`,
    provider: {
      key_rejected: (provider: string) => `${provider} did not accept the AI key. Check the key in Settings, then send the message again.`,
      rate_limited: (provider: string) =>
        `${provider} is not taking more requests from this key right now: the limit or the credit is used up. Wait a minute, or check your account there, then send it again.`,
      provider_unreachable: (provider: string) => `Hotend could not reach ${provider}. Check the internet connection, then send it again.`,
      model_not_found: (provider: string, model: string | null) =>
        model ? `${provider} does not know the model “${model}”. Choose another model in Settings.` : `${provider} does not know this model. Choose another model in Settings.`,
    },
    openSettings: "Open Settings",
  },
  sizes: {
    title: "Sizes",
    groups: { measure: "To measure", choices: "Your choices", fixed: (n: number) => `Fixed · ${n}` },
    guess: "Hotend's guess",
    measured: "You measured",
    standard: "Standard part",
    calibrated: "From your printer",
    range: (min: string, max: string) => `${min} to ${max}`,
    useMeasurement: "Use my measurement",
    change: "Change",
    measureHelp: "Measure it and type what you read. Hotend rebuilds the part around it.",
    // What to measure for a guessed size, by what the requirement behind it is about.
    // Which side of the thing: named in words and drawn on the part, never "this way across".
    side: (axis: 0 | 1 | 2, side: "long" | "short" | null, now: string) =>
      `${axis === 2 ? "its height, bottom to top" : `${side === null ? "the side that runs" : `the ${side} side,`} ${axis === 0 ? "left to right" : "front to back"}`} (${now} now)`,
    ask: {
      inside: (part: string | null, side: string | null) =>
        side === null
          ? `Measure the thing that goes inside ${part ? `the ${part}` : "the part"}, at its largest. Hotend adds the wall around it.`
          : `Measure the thing that goes inside ${part ? `the ${part}` : "the part"}: ${side}, at its largest. Hotend adds the wall around it.`,
      outside: (part: string | null, side: string | null) =>
        side === null
          ? `Measure the space ${part ? `the ${part}` : "the part"} must fit into. Hotend keeps the outside within it.`
          : `Measure the space ${part ? `the ${part}` : "the part"} must fit into: ${side}. Hotend keeps the outside within it.`,
      opening: (part: string | null) =>
        `Measure what must pass through or sit in this opening${part ? ` of the ${part}` : ""}, at its thickest.`,
      other: () => "Measure the real thing this size stands for, and type what you read.",
    },
    moreThanZero: "A measurement is more than 0.",
    notApplied: (still: string) => `Not applied: the part is still ${still}. Send it again or ask Hotend why.`,
    sendAgain: "Send again",
    dismiss: "Dismiss",
    outOfRange: (label: string, min: string, max: string, unit: string) =>
      `${label} goes from ${min} to ${max}${unit ? ` ${unit}` : ""}. Ask Hotend if you need more.`,
    rebuildFailed: (label: string, value: string, still: string) => `${label} ${value} was not built. Still at ${still}.`,
    openComputer: "Open This computer",
    working: "Hotend is working on this",
    readOnly: (n: number) => `You are looking at v${n}. Go back to the current version to change sizes.`,
    refused: (label: string, value: string, reason: string, still: string) =>
      `${label} ${value} does not build: ${reason} Still at ${still}.`,
    refusedDetails: "Details",
    refusedLess: "Less",
    showAll: (n: number) => `Show all ${n} sizes`,
    showFewer: "Show fewer",
    needsKey: "Add an AI key first: Hotend rebuilds the part around your measurement.",
    measuredMessage: (label: string, value: string) =>
      `I measured ${label}: ${value}. Use this value and mark it as measured.`,
    none: "This model has no sizes to change.",
  },
  proof: {
    title: "Proof",
    // Never "10 of 15": that reads as five failures. Problems first, then what passes.
    counts: (counts: { problem: number; pass: number; measure: number; note: number }) =>
      [
        counts.problem > 0 ? problems(counts.problem) : null,
        counts.pass > 0 ? `${counts.pass} pass` : null,
        counts.measure > 0 ? `${counts.measure} to measure` : null,
        counts.note > 0 ? notes(counts.note) : null,
      ]
        .filter((part): part is string => part !== null)
        .join(" · "),
    problems: (n: number) => problems(n),
    passNumber: (n: number) => `${n} pass`,
    rest: (measure: number, note: number) =>
      [measure > 0 ? `${measure} to measure` : null, note > 0 ? notes(note) : null]
        .filter((part): part is string => part !== null)
        .join(" · "),
    busy: "…",
    groups: { must: "What it must do", fit: "How parts fit", prints: "How it prints" },
    solid: { pass: "Solid, in one piece", other: "Not a printable solid" },
    fits: { pass: "Fits the printer", other: "Too big for the printer" },
    wall: { pass: "Walls thick enough", other: "Wall too thin" },
    overhangs: { pass: "Prints without support", other: "Needs support" },
    bed: { pass: "Sits flat on the bed", other: "Little contact with the bed" },
    small: { pass: "No detail too fine", other: "Detail too fine to print" },
    collide: { pass: "Parts do not collide", other: "Parts collide" },
    card: {
      pass: (card: string) => `${card}: goes together, gaps as drawn`,
      off: (card: string) => `${card}: a gap is off`,
      joint: (card: string) => `${card}: cannot be put together`,
    },
    fitFinding: {
      robustness: "Fits not proven on this printer",
      unverified: "A clearance that nothing verifies",
      declaration: "A declared fit could not be checked",
      undeclared: "A joint made by hand, not checked",
    },
    contract: {
      cavity: (part: string) => `Room inside the ${part}`,
      outer: (part: string) => `Outer size of the ${part}`,
      wall: (part: string) => `Wall of the ${part}`,
      dimension: (part: string) => `A size of the ${part}`,
      fit: (a: string, b: string, card: string | null) =>
        card === null ? `Fit between the ${a} and the ${b}` : `Fit between the ${a} and the ${b} (${card})`,
    },
    guess: {
      met: "Meets the guess. Not proven until you measure it.",
      unknown: "Hotend cannot measure this on the part.",
      notChecked: "Not checked yet: measure it and type what you read above.",
      fit: "Goes together as drawn. Not proven on this printer.",
    },
    unmet: {
      cavity: "Does not fit as built. Measuring will not fix this; ask Hotend to make room.",
      other: "Not met as built. Measuring will not fix this; ask Hotend to change the part.",
    },
    verdict: {
      cavity: (part: string, free: string, asked: string) => `The ${part} is too small inside: ${free} mm free, ${asked} mm needed`,
      cavityPlain: (part: string) => `The ${part} is too small inside for what it must hold`,
    },
    summary: {
      wallTooThin: (part: string, thinnest: string, least: string) =>
        `A wall of the ${part} is ${thinnest} mm thick. This printer needs ${least} mm.`,
      cavityFits: (size: string, part: string) => `A ${size} block fits inside the ${part}.`,
      cavityTooSmall: (size: string, part: string, free: string | null) =>
        free === null
          ? `A ${size} block does not fit inside the ${part}: something stands in the way.`
          : `A ${size} block does not fit inside the ${part}: something stands in the way, and only about ${free} mm is free.`,
      outerFits: (part: string, size: string) => `The ${part} stays within ${size}.`,
      outerTooBig: (part: string, size: string, is: string | null) =>
        is === null ? `The ${part} is larger than ${size}.` : `The ${part} is ${is}, larger than ${size}.`,
    },
    details: "Details",
    uncalibrated:
      "Hotend uses general values for fits until your printer is calibrated. Print a fit gauge, measure it, and enter what you read in Settings, Calibration.",
    openCalibration: "Open Calibration",
    // The printer was measured after this version was built: the version does not know yet.
    calibratedSince: "Your printer was measured after this version was built. Rebuild it to use the measurements.",
    rebuildWithMeasurements: "Rebuild with the measurements",
    // Built with the measurements, and some fits are still not sure: why, and what helps.
    // `measured` and `general` name what the printer's table says was measured and what was
    // not ("hole sizes", "peg sizes and shrinkage in height"): never more than was measured.
    calibratedPartly: (n: number, measured: string, general: string | null) =>
      `This version uses your printer's measured ${measured}. ${n === 1 ? "1 fit is" : `${n} fits are`} still not sure to work on every print: one gauge print says where that print landed, not how much the printer varies${general === null ? "" : `, and Hotend still uses general values for ${general}`}.`,
    calibratedTitle: (partly: boolean) =>
      partly ? "Fits partly measured on this printer, not yet proven" : "Fits measured on this printer, not yet proven",
    noContract: "Hotend has not written down what this part must do.",
    askContract: "Ask Hotend to add it",
    askContractText: "Write down what this product must do.",
    tellMeasured: "Tell Hotend what you measured",
    tellMeasuredText: "I measured ",
    fixMessage: (sentence: string) => `Fix this: ${sentence}`,
    skipped: "Not checked",
    status: { ok: "Passes", note: "Note", problem: "Problem", question: "To measure", skipped: "Not checked", busy: "Checking" },
    noRun: "Add an AI key to ask Hotend.",
    whileRun: "Hotend is working on this",
  },
  rail: {
    label: "Versions",
    sameProof: (counts: string) => `Same proof: ${counts}.`,
    partSize: (part: string) => `${part} · size`,
    partWeight: (part: string) => `${part} · weight if solid`,
    dotName: (n: number, how: string) => `Version ${n}, ${how}`,
    dotWritten: "written by Hotend",
    dotSize: "one size changed",
    dotProblems: (n: number) => `has ${problems(n)}`,
    lookAt: (n: number) => `Look at v${n}`,
    use: (n: number) => `Use v${n}`,
    current: "The current version",
    proofCount: (counts: string) => `Proof: ${counts}`,
    collapsed: (n: number) => `+${n - 1}`,
    collapsedHint: (n: number) => (n - 1 === 1 ? "1 earlier value of the same size" : `${n - 1} earlier values of the same size`),
    collapsedName: (n: number) => `the last of ${n} changes to one size`,
    newer: "newer than the current version",
    onStage: "on the stage",
    first: "The first version.",
  },
  // Named versions beside the version rail: made, exported and gone back to by the maker.
  snapshots: {
    label: "Snapshots",
    open: (n: number) => (n === 0 ? "Snapshots" : `Snapshots (${n})`),
    none: "No snapshots yet. A snapshot keeps a version by a name you give it, so you can export it or go back to it later.",
    notAvailable: "Snapshots are not available in this window yet. Make them on the command line: hotend snapshot create <name>.",
    take: "Snapshot",
    takeTitle: (version: number) => `Snapshot of v${version}`,
    name: "Name",
    namePlaceholder: "For example: Lid fits",
    nameHint: (max: number) => `One line, up to ${max} characters.`,
    save: "Save snapshot",
    saving: "Saving…",
    cancel: "Cancel",
    version: (n: number) => `v${n}`,
    printed: "Printed",
    printedHint: "The version had been printed when the snapshot was made: the exact sliced file is kept with it.",
    exportButton: "Export…",
    exportTitle: "Save snapshot files",
    exporting: "Exporting…",
    restore: "Go back to it",
    restoreHint: (n: number) => `Makes v${n} the current version again. Later versions stay.`,
    current: "Current",
    remove: "Delete name",
    removeHint: "Removes the name only. The version and its files stay.",
    removeSure: "Delete the name?",
    keep: "Keep",
    exported: (name: string) => `Exported “${name}” into this folder:`,
    files: (n: number) => (n === 1 ? "1 file" : `${n} files`),
    fileKind: { step: "STEP", "3mf": "3MF", printed: "Printed file", manifest: "snapshot.json" } as Record<string, string>,
    fileSource: { copied: "copied", exported: "exported", written: "written" } as Record<string, string>,
    sources: (copied: number, exported: number) => `${copied} copied as stored, ${exported} made now`,
    printedMissing: "The printed file of this snapshot was not found anywhere. The model files and snapshot.json were saved.",
    invalid: {
      empty: "Give the snapshot a name.",
      long: (max: number) => `A snapshot name has at most ${max} characters.`,
      plain: "A snapshot name is one plain line: no line breaks, tabs or invisible characters.",
      taken: "This project already has a snapshot with that name.",
    },
  },
  prepare: {
    // The core names its stand-in a "fake slicer"; the window's word is stand-in (11).
    standInName: "Hotend's stand-in slicer",
    plate: (n: number, of: number) => `Plate ${n} of ${of}`,
    plateButton: (n: number) => `Show plate ${n}`,
    copies: (part: string) => `Copies of ${part}`,
    turn: "Turn…",
    turnTitle: (part: string) => `How should the ${part} lie on the plate?`,
    copiesLabel: "Copies",
    recommended: "Recommended",
    // A way of lying, by the direction of the part that points up.
    ways: {
      "+z": "As modelled",
      "-z": "Upside down",
      "+x": "On its left side",
      "-x": "On its right side",
      "+y": "On its front",
      "-y": "On its back",
    } as Record<string, string>,
    support: {
      none: "No support needed",
      little: "Almost no support",
      some: "Needs some support",
      much: "Needs a lot of support",
    },
    supportUnder: (areaText: string) => `support under ${areaText}`,
    // The piece's own warning, in words a maker can picture.
    hangs: (faces: number, areaText: string) =>
      faces === 1
        ? `1 face hangs in the air (about ${areaText}) and needs support.`
        : `${faces} faces hang in the air (about ${areaText}) and need support.`,
    supports: {
      label: "Supports",
      none: "None needed",
      needed: (areaText: string) => `Needed under about ${areaText}`,
      standIn: "The stand-in slicer does not make supports.",
      real: "Hotend cannot see whether the slicer added them. Look at its preview before you print.",
    },
    onBed: (areaText: string) => `${areaText} on the bed`,
    tall: (height: string) => `${height} tall`,
    doesNotFit: "Does not fit",
    material: "Material",
    mainNozzle: "main nozzle",
    changeInSettings: "Change in Settings",
    slice: "Slice",
    quality: "Quality",
    qualities: [
      { value: "demo-0.20mm", label: "Standard · 0.20 mm" },
      { value: "demo-0.12mm", label: "Fine · 0.12 mm" },
    ],
    notSliced: "Not sliced yet.",
    changed: "Changed since this slice.",
    previewsOutdated: "Sliced before Hotend drew plate previews. Slice again to hand it to Bambu Connect.",
    notStated: "not stated by the slicer",
    layers: (n: number) => count(n, "layer", "layers"),
    opens: "Prepare opens when the part has no problems.",
    nothing: "There is no part to lay out yet.",
    layingOut: "Laying out the plate…",
    standInTag: "Stand-in slicer",
    slicedPlate: (plate: number, of: number) => `Plate ${plate} of ${of} only. The others are sliced one at a time.`,
    tags: {
      fake_slicer: "Rough numbers, and no file a real printer could print.",
      demo_profile: "Generic profile, not tuned for this printer",
      slicer_version: "This slicer version has not been tested with Hotend",
      reach_unchecked: "The slicer did not confirm the nozzle's reach",
      unverified_printer_facts: "X2D limits not confirmed on a real printer",
    } as Record<string, string>,
    // What the two amber tags mean for the maker, said once under them.
    tagsNote: "Time and weight may be off until Hotend has been tried on a real X2D. Nothing for you to do.",
    sliceFailedPlain: "Slicing failed.",
    sliceFailed: {
      slicer_failed: "Slicing failed.",
      slicer_timeout: "The slicer took too long and was stopped.",
      out_of_reach: "A piece lies outside the nozzle's reach.",
      material_not_allowed: "This material cannot be printed on that nozzle.",
      slicer_version_refused: "This slicer version is too old to be run safely.",
    } as Record<string, string>,
    turned: (turned: string) => turned.charAt(0).toUpperCase() + turned.slice(1),
  },
  print: {
    review: "Review",
    last: "Last print",
    what: (project: string, version: number, plate: number, of = 1) =>
      of > 1 ? `${project} · v${version}, plate ${plate} of ${of}` : `${project} · v${version}, plate ${plate}`,
    notStated: "not stated",
    timeNotStated: "Time not stated",
    about: (time: string) => `About ${time}`,
    nozzle: (id: number | null) => (id === 0 ? "Auxiliary" : "Main"),
    spool: (unit: number, slot: number) => `AMS ${unit}, slot ${slot}`,
    facts: {
      what: "Print",
      layers: "Layers",
      material: "Material",
      nozzle: "Nozzle",
      spool: "Spool",
      // The confirmation dialog writes these three lines in the same words, and a person
      // compares the two before a print starts (src-tauri/src/confirm.rs checks this file).
      // Change them there and here together, never on one side.
      nozzleUpTo: "Nozzle up to",
      bedUpTo: "Bed up to",
      // Shown only for a file that heats the chamber, as in the dialog.
      chamberUpTo: "Chamber up to",
    },
    readFromFile: "Read from the sliced file.",
    placeholder: "This file is a placeholder from the stand-in slicer. It can only run on the simulated printer.",
    morePlates: (n: number) =>
      n === 1 ? "1 more plate to slice and print after this one." : `${n} more plates to slice and print after this one.`,
    sendTo: "Send to",
    simulated: { name: "X2D", under: "A simulated print. Nothing is made.", button: "Start simulated print…" },
    // The button and the sentences say what happens: the checked file is saved in the
    // hand-off folder and, when this Hotend has a real opener, Bambu Connect is asked to
    // import it (the confirmation dialog says the same, `confirm.rs`). The person then
    // chooses the printer and presses Print in Bambu Connect.
    connect: {
      name: "Bambu Connect",
      under:
        "Hotend saves the checked file in your hand-off folder and asks Bambu Connect to import it. You choose the printer and press Print there; Hotend does not control the print.",
      // What the stand-in opener (development and tests) does: it opens nothing.
      underNoOpener: "Hotend saves the checked file in your hand-off folder. This Hotend does not open Bambu Connect: import the file there.",
      button: "Save and open in Bambu Connect…",
      buttonNoOpener: "Save for Bambu Connect…",
      folder: (where: string) => `Saved to ${where}`,
      settings: "Choose the hand-off folder in Settings",
    },
    // The editable Bambu Studio project ("Open in Bambu Studio"). The core makes it from the
    // plate's checked slice and hands it over only after the shell's own confirmation dialog
    // (`confirm.rs` says the same in its title and button: a save, and the opening of Bambu
    // Studio only where this Hotend opens it). Nothing here says the project was checked.
    project: {
      name: "Bambu Studio project (editable)",
      under: "Hotend makes a Bambu Studio project of this plate with its print settings. You change it and print it from Bambu Studio.",
      // Shown wherever this way is offered. The confirmation dialog ends with the same
      // sentence (src-tauri/src/confirm.rs checks this file).
      outside: "A project opened this way is sliced again by Bambu Studio, so it is outside Hotend's check.",
      standIn: "The stand-in slicer makes no Bambu Studio project.",
      what: (plate: number) =>
        `Hotend makes the project of plate ${plate} with Bambu Studio, then slices the project once on its own to test it. This can take a minute.`,
      make: "Make the project",
      making: "Making the project with Bambu Studio…",
      made: (plate: number) => `Project of plate ${plate} made.`,
      where: "Where it goes",
      studio: {
        name: "Open in Bambu Studio",
        under: "Hotend saves the project in your hand-off folder and asks the system to open it with Bambu Studio.",
        underNoOpener: "Hotend saves the project in your hand-off folder. This Hotend does not open Bambu Studio: open the project there yourself.",
        button: "Save and open in Bambu Studio…",
        buttonNoOpener: "Save for Bambu Studio…",
      },
      folder: {
        name: "Save to a folder",
        under: "Hotend saves the project in a folder you choose. Nothing is opened.",
        button: "Save project…",
      },
      pickTitle: "Choose a folder for the Bambu Studio project",
      goesTo: (where: string) => `It goes to ${where}`,
      fileName: (name: string) => `File name: ${name}`,
      notConfirmed: "You closed the dialog without saving. Nothing was saved.",
      saved: "The project was saved:",
      opened: "Bambu Studio was asked to open it.",
      notOpened: "Bambu Studio was not opened. Open the project in Bambu Studio yourself.",
      nothingOpened: "Nothing was opened.",
      again: "Save it again",
    },
    // Numbers of the version taken from a web page that nobody has confirmed, as the
    // confirmation dialog lists them ("From the web: ... (from host, not confirmed)").
    web: {
      title: "Numbers from the web, not confirmed",
      line: (label: string, value: string, unit: string, host: string) => `${label} ${value} ${unit} (from ${host}, not confirmed)`,
      more: (n: number) => `and ${n} more not confirmed`,
    },
    folder: {
      name: "A folder or USB drive",
      under: "You carry the file to the printer and start it there.",
      choose: "Choose a folder…",
      button: "Save file…",
      pickTitle: "Choose a folder or USB drive for the sliced file",
      needFolder: "Choose a folder first.",
    },
    standIn: "The stand-in slicer makes no printable file.",
    useStudio: "Use Bambu Studio for a printable file",
    forReal: {
      title: "To print it for real:",
      steps: ["Choose Bambu Studio in Settings, Slicer.", "Slice again.", "Send with Bambu Connect or save to a USB\u00a0drive."],
      button: "Open Settings, Slicer",
    },
    jobVersion: {
      printing: (job: number, now: number) => `Printing v${job}. Your design is now v${now}.`,
      printed: (job: number, now: number) => `This print is of v${job}. Your design is now v${now}.`,
    },
    inUse: "In use",
    stoppedAt: (simulated: boolean, layer: number | null, percent: number | null) => {
      const what = simulated ? "the simulated print" : "the print";
      // Stopped while the printer was still heating: no layer was laid down.
      if (layer === 0) return `You stopped ${what} before its first layer.`;
      if (layer !== null) return `You stopped ${what} at layer ${layer}.`;
      return percent !== null ? `You stopped ${what} at ${percent}\u00a0%.` : `You stopped ${what}.`;
    },
    stillGuesses: (n: number, labels: string) =>
      n === 1 ? `1 number is still a guess: ${labels}.` : `${n} numbers are still guesses: ${labels}.`,
    aiNote: "Hotend's note",
    blocked: {
      gcode_rejected: "Hotend stopped this file before it reached the printer.",
      no_spool: "The job needs a material that no loaded spool matches.",
      slice_changed: "The sliced file has changed since it was made. Slice it again.",
      plan_changed: "The print settings of this plate changed since it was sliced. Slice it again.",
    } as Record<string, string>,
    finding: (line: number, message: string) =>
      line > 0 ? `Line ${line}: ${message}` : message.replace(/^./, (first) => first.toUpperCase()),
    tookTooLong: "That took longer than two minutes, so nothing was started. Review it again.",
    notConfirmed: "You closed the dialog without starting. Nothing was sent.",
    printing: "Printing",
    starting: "Starting",
    sent: "Hotend sent the job. A print counts as started only when the printer says so.",
    left: (time: string) => `About ${time} left`,
    layer: (layer: number, total: number) => `Layer ${layer} of ${total}`,
    bed: (c: number) => `Bed ${Math.round(c)} °C`,
    stop: "Stop",
    stopSure: "Stop for good?",
    stopAsk: "Stop for good? Press Stop for good to end the print, or Keep printing.",
    // The window's own line for how a print ended; the core's sentence follows, quieter.
    outcome: {
      cancelled: (simulated: boolean) => (simulated ? "Stopped. Nothing was made." : "Stopped. Nothing more will be printed."),
      failed: () => "The printer stopped.",
      not_started: () => "The printer did not start.",
    } as Record<string, (simulated: boolean) => string>,
    outcomePlain: "This print did not finish.",
    keep: "Keep printing",
    backDesign: "Back to Design",
    mayRun: "The print may still be running.",
    // A handed-off print as the printer reports it over the local network (read-only).
    followed: {
      title: "Printer status",
      notSetUp: "Printer status: not set up.",
      setUp: "Set it up in Settings, Printer",
      notThisJob: "Printer status: not this job.",
      notThisJobDetail: (name: string | null) =>
        name ? `The printer reports another job: ${name}.` : "The printer reports no job Hotend handed off.",
      notFollowing: "Hotend is not listening to the printer.",
      follow: "Follow the printer",
      waiting: "Listening. The printer has not reported yet.",
      notAvailable: "Printer status is not available in this window yet.",
      fromPrinter: "From the printer's own reports. Hotend listens and sends the printer nothing.",
      phase: "Phase",
      layer: "Layer",
      layerOf: (layer: number, total: number) => `${layer} of ${total}`,
      progress: "Progress",
      left: "Time left",
      nozzle: (id: number, many: boolean) => (many ? (id === 0 ? "Nozzle (auxiliary)" : id === 1 ? "Nozzle (main)" : `Nozzle ${id}`) : "Nozzle"),
      bed: "Bed",
      chamber: "Chamber",
      temperature: (current: number | null, target: number | null) =>
        current === null ? "not reported" : target ? `${Math.round(current)} °C → ${Math.round(target)} °C` : `${Math.round(current)} °C`,
      problems: "Problems",
      ended: (why: string) => `Hotend stopped listening: ${why}`,
      disconnected: "Not connected to the printer.",
    },
    cannotFollow: "Hotend cannot follow this print.",
    connectAsked: "Bambu Connect was asked to import this file. Choose the printer and press Print there:",
    connectNotOpened: "Bambu Connect was not opened. Import this file in Bambu Connect, choose the printer and press Print:",
    finishedNote: "The simulated print finished. Nothing was made.",
    ams: (unit: number, high: boolean, nozzle: number | null) =>
      `${high ? "AMS HT" : `AMS ${unit}`} · ${nozzle === 0 ? "auxiliary nozzle" : "main nozzle"}`,
    progress: "Progress of the print",
    empty: "Empty",
    spoolLeft: (percent: number) => `${percent} % left`,
    spoolColour: (hex: string) => `Spool colour ${hex}`,
    spoolNoColour: "Spool colour not stated",
  },
  chip: {
    name: "X2D",
    nothing: "Simulated: no real printer is connected.",
    simulatedJob: "Simulated print: nothing is really made.",
    idle: "Idle",
    printing: (percent: number | null) => (percent === null ? "Printing" : `Printing ${percent} %`),
    line: (percent: number | null, minutes: number | null) =>
      `Printing${percent === null ? "" : ` · ${percent} %`}${minutes === null ? "" : ` · about ${duration(minutes * 60)} left`}`,
    what: (project: string, plate: number) => `${project}, plate ${plate}`,
    go: "Go to the print",
    open: "Printer",
  },
} as const;
