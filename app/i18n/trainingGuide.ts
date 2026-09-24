import type { Language } from './translations';

/**
 * The training guide of YuE2 Studio, per language. It follows the studio's own
 * training pipeline and the notes of the HOT-Step trainer it runs.
 */

export interface GuideSection {
  title: string;
  text?: string[];
  steps?: string[];
  list?: string[];
  checklist?: string[];
  examples?: { label?: string; body: string }[];
}

export interface Guide {
  title: string;
  intro?: string;
  expandAll: string;
  collapseAll: string;
  close: string;
  resize: string;
  sections: GuideSection[];
}

const ru: Guide = {
  title: 'Справка по обучению LoRA',
  intro: 'LoRA — небольшое дополнение к модели, которое учится звучать как ваши песни: манера, вокал, аранжировки, продакшн. Окно можно двигать за заголовок и растягивать за правый нижний угол.',
  expandAll: 'Раскрыть всё',
  collapseAll: 'Свернуть всё',
  close: 'Закрыть',
  resize: 'Потяните, чтобы изменить размер',
  sections: [
    {
      title: 'Порядок работы',
      steps: [
        'Скачайте пакет обучения (около 8.6 ГБ, один раз). Нужна NVIDIA RTX 30-й серии или новее и около 11 ГБ видеопамяти.',
        'Создайте набор: название и слово-триггер — редкое слово, которого нет в обычных описаниях (например, имя латиницей без пробелов).',
        'Добавьте песни одного исполнителя или одного стиля — из библиотеки или с диска (WAV, MP3, FLAC, OGG, M4A). Песни короче 10 секунд не принимаются.',
        'У каждой песни заполните «Стиль» и «Текст» (ниже — что писать). Инструментал отметьте галочкой.',
        'Проверьте, что у всех песен нет жёлтого треугольника — он значит, что стиль или текст пустые.',
        'Запустите обучение. Пока оно идёт, генерация, ассистент, караоке и разделение на дорожки недоступны.',
        'Когда запуск закончится, послушайте чекпоинты и нажмите «В LoRA» под лучшим — он появится на странице LoRA.',
      ],
    },
    {
      title: 'Какие песни брать',
      list: [
        'Один исполнитель или один узкий стиль. Сборная солянка учит «ничему конкретному».',
        'Обычно берут от 5 до 20 песен. Автор тренера не заметил надёжной разницы между 10 и 20 треками; плотные по тексту или разностилевые альбомы учатся тяжелее.',
        'Ровное качество записей: студийные версии, без концертных шумов, радио-джинглов и обрезанных фрагментов.',
        'Целые песни: тренер сам режет их на кусочки по 10 секунд.',
        'Не добавляйте одну и ту же песню дважды (оригинал и ремастер) — это перекос в её сторону.',
      ],
    },
    {
      title: 'Стиль — что писать',
      text: [
        'Одно предложение на английском, примерно 35–70 слов, в таком порядке: язык → жанр и эпоха → вокал → инструменты → настроение → продакшн → темп «N BPM».',
        'Не пишите имя исполнителя, название песни, тональность и размер, не цитируйте текст песни. Для инструментала вместо языка пишите «instrumental».',
        'BPM лучше взять из анализатора или сервиса с базой треков, а не на глаз: неверный темп учит неверному.',
        'Слово-триггер в стиль писать не нужно — студия добавит его сама.',
      ],
      examples: [
        {
          label: 'Пример',
          body: 'Russian-language 2010s indie pop with bright synth pop touches, young female lead vocal with a playful ironic delivery and light doubled harmonies, analog synths, drum machine and clean electric guitar, carefree yet slightly melancholic mood, crisp modern bedroom-pop production with a warm low end, 120 BPM',
        },
      ],
    },
    {
      title: 'Текст песни',
      list: [
        'Точно те слова, что поются, — без аккордов, ссылок и примечаний. Текст с сайтов обязательно сверьте с записью.',
        'Размечайте части: [Verse 1], [Chorus], [Verse 2], [Bridge], [Outro] — каждая часть с новой строки.',
        'Файл .txt или .lrc с тем же именем, что и аудио, подхватывается при добавлении; таймкоды из .lrc убираются.',
        'Кнопка с микрофоном распознаёт текст: отделяет вокал, распознаёт речь (движок выбирается в «Настройки → Караоке») и раскладывает строки по частям. Результат всегда проверяйте.',
        'Осторожно: песня без текста сразу считается инструменталом, и «Распознать все тексты» её пропустит. Снимите галочку «Инструментал» или нажмите микрофон у этой песни.',
      ],
    },
    {
      title: 'Что студия делает сама',
      list: [
        'Переводит всё в WAV 48 кГц и режет на 10-секундные кусочки.',
        'Отделяет вокал (вместе с бэк-вокалом) у песен с текстом.',
        'Привязывает слова к времени по вокалу — модель учит, где какое слово поётся.',
        'Строит партитуру каждой песни (SheetSage2).',
        'Подставляет слово-триггер в обучение, а при генерации — в стиль, когда вы выбираете эту LoRA.',
        'Останавливает обучение сама, когда LoRA достаточно похожа (по метрике KL), и сохраняет чекпоинты.',
      ],
    },
    {
      title: 'Что нужно сделать самому',
      list: [
        'Подобрать песни и проверить их качество.',
        'Написать стиль каждой песни — у YuE2 Studio нет кнопки «Описать», и ассистент песни не слушает.',
        'Проверить и поправить тексты: распознавание ошибается, сайты с текстами тоже.',
        'Узнать темп (BPM) — студия его не измеряет.',
        'Выбрать чекпоинт на слух.',
      ],
    },
    {
      title: 'Настройки запуска',
      text: ['Настройки по умолчанию — рецепт автора тренера HOT-Step. Без причины их лучше не трогать.'],
      list: [
        'Остановить на KL = 1.4. Сходство с исполнителем начинается примерно с 1.25, около 1.9 модель начинает портиться (зацикленные концовки). Значение одинаково для любого исполнителя.',
        'Предел шагов 750 — это потолок, а не цель: если к нему KL не дошёл до 1.4, дальше обычно не дойдёт.',
        'Сохранять каждые 50 шагов — будет из чего выбрать; последний чекпоинт сохраняется в момент остановки.',
        'LoKr 64 / фактор 4 / alpha 256 и оптимизатор Prodigy — лёгкий адаптер (около 106 МБ) с подбором скорости обучения.',
        'Тайминги текста включены: они учат модель попадать словами в музыку. Для них нужен установленный разделитель вокала.',
      ],
    },
    {
      title: 'Выбор чекпоинта и генерация',
      list: [
        'Выбирайте на слух, а не по графику ошибки: сгенерируйте одну и ту же песню с разными чекпоинтами.',
        'Нажмите «В LoRA» под нужным шагом — LoRA появится на странице LoRA с именем «запуск · шаг».',
        'При выборе LoRA на странице «Создать» триггер подставится сам.',
        'У LoRA две силы: композиция (AR) и звук (NAR), по умолчанию 1 и 1. Автор тренера на слух получал лучший звук при NAR около 2.',
      ],
    },
    {
      title: 'Если что-то не так',
      list: [
        'Песни зацикливаются, нет концовки, вокал разваливается — LoRA перетренирована: возьмите более ранний чекпоинт или уменьшите силу.',
        'LoRA почти ничего не меняет — возьмите более поздний чекпоинт, проверьте стиль и тексты, добавьте песен.',
        'Слова «съезжают» с музыки — проверьте тексты: лишние строки и повторы, которых нет в записи, сбивают тайминги.',
        'Обучение не стартует с ошибкой про разделитель — установите разделитель вокала или выключите тайминги текста.',
      ],
    },
    {
      title: 'Чеклист перед запуском',
      checklist: [
        'Песни одного исполнителя или стиля, 5–20 штук, хорошего качества.',
        'Нет дублей и обрезков.',
        'У каждой песни стиль одним предложением на английском, с BPM, без имени и названия.',
        'Тексты сверены с записью и размечены [Verse] / [Chorus].',
        'Инструменталы отмечены галочкой, у песен с вокалом галочка снята.',
        'Нет жёлтых треугольников.',
        'Задано редкое слово-триггер.',
        'Свободно около 11 ГБ видеопамяти, генерация остановлена.',
      ],
    },
  ],
};

const en: Guide = {
  title: 'LoRA training guide',
  intro: 'A LoRA is a small add-on to the model that learns to sound like your songs: the manner, the vocals, the arrangements, the production. Drag this window by its title and resize it from the bottom-right corner.',
  expandAll: 'Expand all',
  collapseAll: 'Collapse all',
  close: 'Close',
  resize: 'Drag to resize',
  sections: [
    {
      title: 'Workflow',
      steps: [
        'Download the training pack (about 8.6 GB, once). It needs an NVIDIA RTX 30-series card or newer with about 11 GB of VRAM.',
        'Create a dataset: a name and a trigger word — a rare word that never appears in ordinary descriptions (for example a name in Latin letters, no spaces).',
        'Add songs of one artist or one style, from the library or from disk (WAV, MP3, FLAC, OGG, M4A). Songs shorter than 10 seconds are refused.',
        'Fill in Style and Lyrics for every song (what to write is below). Tick Instrumental for instrumentals.',
        'Make sure no song shows the yellow triangle — it means the style or the lyrics are empty.',
        'Start training. While it runs, generation, the assistant, karaoke and stem separation are unavailable.',
        'When the run ends, listen to the checkpoints and press "To LoRA" under the best one — it appears on the LoRA page.',
      ],
    },
    {
      title: 'Which songs to use',
      list: [
        'One artist or one narrow style. A mixed bag teaches nothing in particular.',
        'Usually 5 to 20 songs. The trainer\'s author saw no reliable difference between 10 and 20 tracks; lyric-dense or mixed-style albums are harder to learn.',
        'Even recording quality: studio versions, no live noise, radio jingles or cut-off fragments.',
        'Whole songs: the trainer cuts them into 10-second pieces itself.',
        'Do not add the same song twice (original and remaster) — it tilts the LoRA towards it.',
      ],
    },
    {
      title: 'Style — what to write',
      text: [
        'One English sentence of about 35–70 words, in this order: language → genre and era → vocals → instruments → mood → production → tempo as "N BPM".',
        'Do not name the artist or the song, do not give the key or the time signature, do not quote the lyrics. For an instrumental write "instrumental" in place of the language.',
        'Take the BPM from an analyser or a track database rather than guessing: a wrong tempo teaches the wrong thing.',
        'Do not write the trigger word in the style — the studio adds it itself.',
      ],
      examples: [
        {
          label: 'Example',
          body: 'Russian-language 2010s indie pop with bright synth pop touches, young female lead vocal with a playful ironic delivery and light doubled harmonies, analog synths, drum machine and clean electric guitar, carefree yet slightly melancholic mood, crisp modern bedroom-pop production with a warm low end, 120 BPM',
        },
      ],
    },
    {
      title: 'Lyrics',
      list: [
        'Exactly the words that are sung — no chords, links or notes. Lyrics from websites must be checked against the recording.',
        'Mark the parts: [Verse 1], [Chorus], [Verse 2], [Bridge], [Outro], each on its own line.',
        'A .txt or .lrc file with the same name as the audio is picked up when adding; .lrc timestamps are removed.',
        'The microphone button recognises the lyrics: it separates the vocals, transcribes them (the engine is chosen in Settings → Karaoke) and lays the lines out by part. Always check the result.',
        'Careful: a song without lyrics counts as instrumental at once, and "Recognise all lyrics" skips it. Untick Instrumental or press the microphone on that song.',
      ],
    },
    {
      title: 'What the studio does itself',
      list: [
        'Converts everything to 48 kHz WAV and cuts it into 10-second pieces.',
        'Separates the vocals (backing vocals included) of songs with lyrics.',
        'Aligns the words to the vocals in time — the model learns where each word is sung.',
        'Builds a score of every song (SheetSage2).',
        'Adds the trigger word in training, and to the style at generation when you pick this LoRA.',
        'Stops training by itself once the LoRA is close enough (by the KL metric), and saves checkpoints.',
      ],
    },
    {
      title: 'What you have to do yourself',
      list: [
        'Choose the songs and check their quality.',
        'Write each song\'s style — YuE2 Studio has no Describe button, and the assistant does not listen to songs.',
        'Check and fix the lyrics: recognition makes mistakes, lyrics sites do too.',
        'Find out the tempo (BPM) — the studio does not measure it.',
        'Pick the checkpoint by ear.',
      ],
    },
    {
      title: 'Run settings',
      text: ['The defaults are the recipe of the HOT-Step trainer\'s author. Leave them alone without a reason.'],
      list: [
        'Stop at KL = 1.4. Likeness to the artist starts around 1.25; around 1.9 the model starts to degrade (looping endings). The value means the same for any artist.',
        'The 750-step limit is a ceiling, not a target: if KL has not reached 1.4 by then, it usually will not.',
        'Save every 50 steps — there will be something to choose from; the last checkpoint is saved when the run stops.',
        'LoKr 64 / factor 4 / alpha 256 with the Prodigy optimiser — a light adapter (about 106 MB) that finds its own learning rate.',
        'Lyric timing is on: it teaches the model to land the words on the music. It needs the vocal separator installed.',
      ],
    },
    {
      title: 'Choosing a checkpoint and generating',
      list: [
        'Choose by ear, not by the loss chart: generate the same song with different checkpoints.',
        'Press "To LoRA" under the step you want — the LoRA appears on the LoRA page as "run · step".',
        'When you pick the LoRA on the Create page, the trigger is added by itself.',
        'The LoRA has two strengths: composition (AR) and sound (NAR), 1 and 1 by default. The trainer\'s author heard the best sound with NAR around 2.',
      ],
    },
    {
      title: 'When something is wrong',
      list: [
        'Songs loop, have no ending, the vocal falls apart — the LoRA is overtrained: take an earlier checkpoint or lower the strength.',
        'The LoRA changes almost nothing — take a later checkpoint, check the styles and lyrics, add songs.',
        'Words drift off the music — check the lyrics: extra lines and repeats that are not in the recording throw the timing off.',
        'Training will not start with an error about the separator — install the vocal separator or turn lyric timing off.',
      ],
    },
    {
      title: 'Checklist before a run',
      checklist: [
        'Songs of one artist or style, 5–20 of them, of good quality.',
        'No duplicates or fragments.',
        'Every song has a one-sentence English style with BPM, without the artist or the title.',
        'Lyrics checked against the recording and marked [Verse] / [Chorus].',
        'Instrumentals ticked, songs with vocals unticked.',
        'No yellow triangles.',
        'A rare trigger word is set.',
        'About 11 GB of VRAM free, generation stopped.',
      ],
    },
  ],
};

const zh: Guide = {
  title: 'LoRA 训练指南',
  intro: 'LoRA 是模型的一个小附加件，它学习你的歌曲的声音：演唱方式、人声、编曲和制作。可以拖动标题栏移动窗口，拖动右下角调整大小。',
  expandAll: '全部展开',
  collapseAll: '全部收起',
  close: '关闭',
  resize: '拖动以调整大小',
  sections: [
    {
      title: '操作流程',
      steps: [
        '下载训练包（约 8.6 GB，只需一次）。需要 NVIDIA RTX 30 系列或更新的显卡，约 11 GB 显存。',
        '创建数据集：名称和触发词——一个普通描述里不会出现的罕见词（例如拉丁字母、无空格的名字）。',
        '添加同一艺人或同一风格的歌曲，可来自曲库或磁盘（WAV、MP3、FLAC、OGG、M4A）。短于 10 秒的歌曲会被拒绝。',
        '为每首歌填写“风格”和“歌词”（写法见下文）。纯音乐请勾选“纯音乐”。',
        '确认没有歌曲显示黄色三角——它表示风格或歌词为空。',
        '开始训练。训练期间无法生成歌曲、使用助手、卡拉OK和分轨。',
        '训练结束后试听各检查点，在最好的一个下点击“加入 LoRA”——它会出现在 LoRA 页面。',
      ],
    },
    {
      title: '选择哪些歌曲',
      list: [
        '同一艺人或同一窄风格。大杂烩学不到任何具体的东西。',
        '通常 5 到 20 首。训练器作者没有发现 10 首和 20 首之间有可靠差别；歌词密集或风格混杂的专辑更难学。',
        '录音质量一致：录音室版本，不要现场噪音、电台片头或截断的片段。',
        '使用完整歌曲：训练器会自己切成 10 秒的片段。',
        '不要把同一首歌加两次（原版和重制版）——会让 LoRA 偏向它。',
      ],
    },
    {
      title: '风格——写什么',
      text: [
        '一句英文，约 35–70 个词，按此顺序：语言 → 流派和年代 → 人声 → 乐器 → 情绪 → 制作 → 速度“N BPM”。',
        '不要写艺人名、歌名、调性和拍号，不要引用歌词。纯音乐在语言的位置写“instrumental”。',
        'BPM 最好来自分析工具或曲库，不要凭感觉：错误的速度会教错东西。',
        '不要在风格里写触发词——工作室会自动添加。',
      ],
      examples: [
        {
          label: '示例',
          body: 'Russian-language 2010s indie pop with bright synth pop touches, young female lead vocal with a playful ironic delivery and light doubled harmonies, analog synths, drum machine and clean electric guitar, carefree yet slightly melancholic mood, crisp modern bedroom-pop production with a warm low end, 120 BPM',
        },
      ],
    },
    {
      title: '歌词',
      list: [
        '只写实际唱出的词——不要和弦、链接或注释。来自网站的歌词必须对照录音核对。',
        '标注段落：[Verse 1]、[Chorus]、[Verse 2]、[Bridge]、[Outro]，每段单独一行。',
        '与音频同名的 .txt 或 .lrc 文件会在添加时自动读取；.lrc 的时间戳会被去掉。',
        '麦克风按钮会识别歌词：分离人声、转写（引擎在“设置 → 卡拉OK”中选择），并按段落排列。请务必检查结果。',
        '注意：没有歌词的歌曲会立即被视为纯音乐，“识别全部歌词”会跳过它。请取消“纯音乐”勾选，或对这首歌点击麦克风。',
      ],
    },
    {
      title: '工作室自动完成的事',
      list: [
        '把所有音频转成 48 kHz WAV 并切成 10 秒片段。',
        '为有歌词的歌曲分离人声（包括和声）。',
        '按人声把歌词对齐到时间——模型学习每个词在哪里唱出。',
        '为每首歌生成乐谱（SheetSage2）。',
        '训练时加入触发词；选用此 LoRA 生成时自动把触发词加到风格里。',
        '当 LoRA 足够相似（按 KL 指标）时自动停止训练，并保存检查点。',
      ],
    },
    {
      title: '需要你自己做的事',
      list: [
        '挑选歌曲并检查质量。',
        '为每首歌写风格——YuE2 Studio 没有“描述”按钮，助手也不会听歌。',
        '检查并修改歌词：识别会出错，歌词网站也会。',
        '查出速度（BPM）——工作室不会测量。',
        '凭耳朵选择检查点。',
      ],
    },
    {
      title: '训练设置',
      text: ['默认值是 HOT-Step 训练器作者的配方。没有理由时不要改动。'],
      list: [
        'KL = 1.4 时停止。约 1.25 开始像这位艺人，约 1.9 模型开始变差（结尾循环）。这个值对任何艺人都一样。',
        '750 步上限是天花板而不是目标：如果到那时 KL 还没到 1.4，通常也不会再到。',
        '每 50 步保存一次——这样有得选；停止时会保存最后一个检查点。',
        'LoKr 64 / 因子 4 / alpha 256，配 Prodigy 优化器——轻量适配器（约 106 MB），会自己找学习率。',
        '歌词对齐已开启：它教模型让歌词落在音乐上。需要已安装人声分离器。',
      ],
    },
    {
      title: '选择检查点与生成',
      list: [
        '凭耳朵选择，而不是看损失曲线：用不同检查点生成同一首歌对比。',
        '在想要的步数下点击“加入 LoRA”——它会以“训练 · 步数”的名字出现在 LoRA 页面。',
        '在“创建”页选择这个 LoRA 时，触发词会自动加入。',
        'LoRA 有两个强度：作曲（AR）和声音（NAR），默认都是 1。训练器作者听感上 NAR 约为 2 时声音最好。',
      ],
    },
    {
      title: '出现问题时',
      list: [
        '歌曲循环、没有结尾、人声散架——LoRA 训练过头了：换更早的检查点或降低强度。',
        'LoRA 几乎没有变化——换更晚的检查点，检查风格和歌词，增加歌曲。',
        '歌词跟音乐对不上——检查歌词：录音里没有的多余行和重复会打乱对齐。',
        '训练因分离器报错无法开始——安装人声分离器或关闭歌词对齐。',
      ],
    },
    {
      title: '开始训练前的检查清单',
      checklist: [
        '同一艺人或风格的歌曲 5–20 首，质量良好。',
        '没有重复或残缺片段。',
        '每首歌都有一句英文风格，含 BPM，不含艺人名和歌名。',
        '歌词已对照录音核对，并标注 [Verse] / [Chorus]。',
        '纯音乐已勾选，有人声的歌未勾选。',
        '没有黄色三角。',
        '已设置罕见的触发词。',
        '约 11 GB 显存空闲，已停止生成。',
      ],
    },
  ],
};

const ja: Guide = {
  title: 'LoRA 学習ガイド',
  intro: 'LoRA はモデルへの小さな追加で、あなたの曲の響き（歌い方、ボーカル、アレンジ、プロダクション）を学びます。タイトルバーをドラッグして移動、右下の角でサイズを変えられます。',
  expandAll: 'すべて開く',
  collapseAll: 'すべて閉じる',
  close: '閉じる',
  resize: 'ドラッグでサイズ変更',
  sections: [
    {
      title: '作業の流れ',
      steps: [
        '学習パックをダウンロード（約 8.6 GB、一度だけ）。NVIDIA RTX 30 シリーズ以降と約 11 GB の VRAM が必要です。',
        'データセットを作成：名前とトリガーワード（普通の説明には出てこない珍しい単語。例：スペースなしのローマ字の名前）。',
        '同じアーティストまたは同じスタイルの曲を、ライブラリかディスクから追加（WAV、MP3、FLAC、OGG、M4A）。10 秒未満の曲は受け付けません。',
        '各曲の「スタイル」と「歌詞」を記入（書き方は下記）。インストは「インスト」にチェック。',
        '黄色い三角が出ている曲がないか確認。スタイルか歌詞が空という意味です。',
        '学習を開始。学習中は生成、アシスタント、カラオケ、ステム分離が使えません。',
        '終わったらチェックポイントを聴き比べ、一番良いものの下で「LoRA に追加」を押すと LoRA ページに表示されます。',
      ],
    },
    {
      title: 'どの曲を使うか',
      list: [
        '一人のアーティストか、狭い一つのスタイル。寄せ集めでは何も具体的に学びません。',
        '通常 5〜20 曲。学習器の作者は 10 曲と 20 曲で確かな差を見ていません。歌詞の多いアルバムや作風がばらばらなアルバムは学びにくいです。',
        '録音品質をそろえる：スタジオ版で、ライブの雑音、ラジオのジングル、途切れた断片は避けます。',
        '曲はまるごと：学習器が自分で 10 秒ずつに切ります。',
        '同じ曲を二度入れない（オリジナルとリマスターなど）。その曲に偏ります。',
      ],
    },
    {
      title: 'スタイル — 何を書くか',
      text: [
        '英語で一文、約 35〜70 語。順番は 言語 → ジャンルと年代 → ボーカル → 楽器 → ムード → プロダクション → テンポ「N BPM」。',
        'アーティスト名、曲名、キー、拍子は書かず、歌詞も引用しません。インストは言語の代わりに「instrumental」と書きます。',
        'BPM は推測せず、解析ツールや曲データベースから取ります。間違ったテンポは間違いを学ばせます。',
        'トリガーワードはスタイルに書かないでください。スタジオが自動で加えます。',
      ],
      examples: [
        {
          label: '例',
          body: 'Russian-language 2010s indie pop with bright synth pop touches, young female lead vocal with a playful ironic delivery and light doubled harmonies, analog synths, drum machine and clean electric guitar, carefree yet slightly melancholic mood, crisp modern bedroom-pop production with a warm low end, 120 BPM',
        },
      ],
    },
    {
      title: '歌詞',
      list: [
        '実際に歌われている言葉だけ。コード、リンク、注釈は入れません。サイトの歌詞は必ず録音と照らし合わせます。',
        'パートを付ける：[Verse 1]、[Chorus]、[Verse 2]、[Bridge]、[Outro]、それぞれ別の行に。',
        '音声と同じ名前の .txt / .lrc は追加時に読み込まれ、.lrc のタイムスタンプは取り除かれます。',
        'マイクボタンで歌詞を認識：ボーカルを分離して文字起こし（エンジンは「設定 → カラオケ」で選択）、パートごとに並べます。結果は必ず確認してください。',
        '注意：歌詞のない曲はすぐインスト扱いになり、「すべての歌詞を認識」で飛ばされます。「インスト」のチェックを外すか、その曲のマイクを押してください。',
      ],
    },
    {
      title: 'スタジオが自動でやること',
      list: [
        'すべて 48 kHz の WAV に変換し、10 秒ずつに切ります。',
        '歌詞のある曲のボーカル（コーラスを含む）を分離します。',
        'ボーカルに合わせて単語を時間に揃えます。モデルはどの言葉がどこで歌われるかを学びます。',
        '各曲の楽譜を作ります（SheetSage2）。',
        '学習ではトリガーワードを加え、この LoRA を選んで生成するときもスタイルに加えます。',
        'LoRA が十分に似たら（KL 指標で）自動で止め、チェックポイントを保存します。',
      ],
    },
    {
      title: '自分でやること',
      list: [
        '曲を選び、品質を確認する。',
        '各曲のスタイルを書く。YuE2 Studio には「説明」ボタンがなく、アシスタントは曲を聴きません。',
        '歌詞を確認・修正する。認識も歌詞サイトも間違えます。',
        'テンポ（BPM）を調べる。スタジオは測りません。',
        'チェックポイントを耳で選ぶ。',
      ],
    },
    {
      title: '学習の設定',
      text: ['既定値は HOT-Step 学習器の作者のレシピです。理由がなければ変えないでください。'],
      list: [
        'KL = 1.4 で停止。1.25 前後からアーティストに似始め、1.9 前後でモデルが崩れ始めます（エンディングのループ）。この値はどのアーティストでも同じ意味です。',
        '750 ステップは上限であって目標ではありません。そこまでに KL が 1.4 に届かなければ、たいていその先も届きません。',
        '50 ステップごとに保存。選べる候補が残り、止まった時点の最後のチェックポイントも保存されます。',
        'LoKr 64 / 係数 4 / alpha 256 と Prodigy オプティマイザ。軽いアダプタ（約 106 MB）で、学習率を自分で見つけます。',
        '歌詞タイミングはオン。言葉を音楽に合わせることを学ばせます。ボーカル分離器のインストールが必要です。',
      ],
    },
    {
      title: 'チェックポイントの選び方と生成',
      list: [
        '損失グラフではなく耳で選びます。同じ曲を別々のチェックポイントで生成して比べます。',
        '選んだステップの下で「LoRA に追加」を押すと、「実行 · ステップ」という名前で LoRA ページに出ます。',
        '「作成」ページでこの LoRA を選ぶと、トリガーは自動で加わります。',
        'LoRA の強さは作曲（AR）と音（NAR）の二つで、既定は 1 と 1。学習器の作者は NAR を 2 前後にしたとき一番良い音を聴いています。',
      ],
    },
    {
      title: 'うまくいかないとき',
      list: [
        '曲がループする、終わらない、ボーカルが崩れる — 学習しすぎです。前のチェックポイントにするか強さを下げます。',
        'LoRA でほとんど変わらない — 後のチェックポイントにし、スタイルと歌詞を確認し、曲を増やします。',
        '言葉が音楽からずれる — 歌詞を確認。録音にない余分な行や繰り返しがタイミングを乱します。',
        '分離器のエラーで学習が始まらない — ボーカル分離器を入れるか、歌詞タイミングをオフにします。',
      ],
    },
    {
      title: '開始前のチェックリスト',
      checklist: [
        '同じアーティストかスタイルの曲が 5〜20 曲、品質良好。',
        '重複や断片がない。',
        '各曲に英語一文のスタイル（BPM 付き、アーティスト名と曲名なし）。',
        '歌詞を録音と照合し、[Verse] / [Chorus] を付けた。',
        'インストにはチェック、ボーカル曲はチェックなし。',
        '黄色い三角がない。',
        '珍しいトリガーワードを設定した。',
        'VRAM が約 11 GB 空いていて、生成は止めてある。',
      ],
    },
  ],
};

const ko: Guide = {
  title: 'LoRA 학습 안내',
  intro: 'LoRA는 모델에 붙는 작은 추가 파일로, 당신의 곡처럼 들리는 법(창법, 보컬, 편곡, 프로덕션)을 배웁니다. 제목 표시줄을 끌어 옮기고, 오른쪽 아래 모서리로 크기를 바꿀 수 있습니다.',
  expandAll: '모두 펼치기',
  collapseAll: '모두 접기',
  close: '닫기',
  resize: '끌어서 크기 조절',
  sections: [
    {
      title: '작업 순서',
      steps: [
        '학습 팩을 내려받습니다(약 8.6 GB, 한 번만). NVIDIA RTX 30 시리즈 이상과 약 11 GB VRAM이 필요합니다.',
        '데이터셋을 만듭니다: 이름과 트리거 단어(일반 설명에 나오지 않는 드문 단어, 예: 띄어쓰기 없는 로마자 이름).',
        '같은 아티스트나 같은 스타일의 곡을 라이브러리나 디스크에서 추가합니다(WAV, MP3, FLAC, OGG, M4A). 10초보다 짧은 곡은 받지 않습니다.',
        '각 곡의 「스타일」과 「가사」를 채웁니다(작성법은 아래). 연주곡은 「연주곡」에 체크합니다.',
        '노란 삼각형이 있는 곡이 없는지 확인합니다. 스타일이나 가사가 비어 있다는 뜻입니다.',
        '학습을 시작합니다. 학습 중에는 생성, 어시스턴트, 가라오케, 스템 분리를 쓸 수 없습니다.',
        '끝나면 체크포인트를 들어 보고 가장 좋은 것 아래의 「LoRA에 추가」를 누르면 LoRA 페이지에 나타납니다.',
      ],
    },
    {
      title: '어떤 곡을 쓸까',
      list: [
        '한 아티스트 또는 좁은 한 가지 스타일. 뒤섞인 모음은 아무것도 구체적으로 배우지 못합니다.',
        '보통 5~20곡. 학습기 제작자는 10곡과 20곡 사이에 뚜렷한 차이를 보지 못했습니다. 가사가 빽빽하거나 스타일이 섞인 앨범은 배우기 더 어렵습니다.',
        '녹음 품질을 고르게: 스튜디오 버전, 라이브 잡음·라디오 징글·잘린 조각은 제외합니다.',
        '곡은 통째로: 학습기가 알아서 10초씩 자릅니다.',
        '같은 곡을 두 번 넣지 마세요(원곡과 리마스터 등). 그 곡 쪽으로 치우칩니다.',
      ],
    },
    {
      title: '스타일 — 무엇을 쓸까',
      text: [
        '영어 한 문장, 약 35~70 단어, 순서: 언어 → 장르와 시대 → 보컬 → 악기 → 분위기 → 프로덕션 → 템포 「N BPM」.',
        '아티스트 이름, 곡 제목, 조성, 박자는 쓰지 말고 가사도 인용하지 않습니다. 연주곡은 언어 자리에 「instrumental」이라고 씁니다.',
        'BPM은 짐작하지 말고 분석 도구나 곡 데이터베이스에서 가져옵니다. 틀린 템포는 틀린 것을 가르칩니다.',
        '트리거 단어는 스타일에 쓰지 마세요. 스튜디오가 알아서 넣습니다.',
      ],
      examples: [
        {
          label: '예시',
          body: 'Russian-language 2010s indie pop with bright synth pop touches, young female lead vocal with a playful ironic delivery and light doubled harmonies, analog synths, drum machine and clean electric guitar, carefree yet slightly melancholic mood, crisp modern bedroom-pop production with a warm low end, 120 BPM',
        },
      ],
    },
    {
      title: '가사',
      list: [
        '실제로 부르는 말만. 코드, 링크, 메모는 넣지 않습니다. 사이트에서 가져온 가사는 반드시 녹음과 대조합니다.',
        '파트를 표시합니다: [Verse 1], [Chorus], [Verse 2], [Bridge], [Outro] — 각각 새 줄에.',
        '오디오와 같은 이름의 .txt / .lrc 파일은 추가할 때 읽히며, .lrc 타임스탬프는 제거됩니다.',
        '마이크 버튼은 가사를 인식합니다: 보컬을 분리하고 받아 적은 뒤(엔진은 「설정 → 가라오케」에서 선택) 파트별로 정리합니다. 결과는 꼭 확인하세요.',
        '주의: 가사가 없는 곡은 바로 연주곡으로 간주되어 「모든 가사 인식」에서 건너뜁니다. 「연주곡」 체크를 풀거나 그 곡의 마이크를 누르세요.',
      ],
    },
    {
      title: '스튜디오가 알아서 하는 일',
      list: [
        '모두 48 kHz WAV로 바꾸고 10초 조각으로 자릅니다.',
        '가사가 있는 곡의 보컬(백보컬 포함)을 분리합니다.',
        '보컬에 맞춰 단어를 시간에 맞춥니다. 모델은 어떤 말이 어디서 불리는지 배웁니다.',
        '각 곡의 악보를 만듭니다(SheetSage2).',
        '학습에 트리거 단어를 넣고, 이 LoRA로 생성할 때도 스타일에 넣습니다.',
        'LoRA가 충분히 비슷해지면(KL 지표) 알아서 멈추고 체크포인트를 저장합니다.',
      ],
    },
    {
      title: '직접 해야 하는 일',
      list: [
        '곡을 고르고 품질을 확인합니다.',
        '각 곡의 스타일을 씁니다. YuE2 Studio에는 「설명」 버튼이 없고, 어시스턴트는 곡을 듣지 않습니다.',
        '가사를 확인하고 고칩니다. 인식도 가사 사이트도 틀립니다.',
        '템포(BPM)를 알아냅니다. 스튜디오는 재지 않습니다.',
        '체크포인트를 귀로 고릅니다.',
      ],
    },
    {
      title: '학습 설정',
      text: ['기본값은 HOT-Step 학습기 제작자의 레시피입니다. 이유 없이 바꾸지 마세요.'],
      list: [
        'KL = 1.4에서 멈춤. 1.25 부근부터 아티스트와 닮기 시작하고, 1.9 부근에서 모델이 망가지기 시작합니다(엔딩 반복). 이 값은 어느 아티스트에게나 같은 의미입니다.',
        '750 스텝은 목표가 아니라 상한입니다. 그때까지 KL이 1.4에 닿지 않으면 보통 그 뒤에도 닿지 않습니다.',
        '50 스텝마다 저장 — 고를 거리가 남고, 멈춘 순간의 마지막 체크포인트도 저장됩니다.',
        'LoKr 64 / 계수 4 / alpha 256과 Prodigy 옵티마이저 — 가벼운 어댑터(약 106 MB)로 학습률을 스스로 찾습니다.',
        '가사 타이밍이 켜져 있습니다. 말을 음악에 맞추는 법을 가르칩니다. 보컬 분리기가 설치되어 있어야 합니다.',
      ],
    },
    {
      title: '체크포인트 고르기와 생성',
      list: [
        '손실 그래프가 아니라 귀로 고릅니다. 같은 곡을 여러 체크포인트로 생성해 비교하세요.',
        '원하는 스텝 아래의 「LoRA에 추가」를 누르면 「실행 · 스텝」 이름으로 LoRA 페이지에 나타납니다.',
        '「만들기」 페이지에서 이 LoRA를 고르면 트리거가 알아서 들어갑니다.',
        'LoRA 강도는 작곡(AR)과 소리(NAR) 두 가지이고 기본은 1과 1입니다. 학습기 제작자는 NAR 약 2에서 가장 좋은 소리를 들었습니다.',
      ],
    },
    {
      title: '문제가 있을 때',
      list: [
        '곡이 반복되거나 끝나지 않거나 보컬이 무너지면 과학습입니다. 더 이른 체크포인트를 쓰거나 강도를 낮추세요.',
        'LoRA가 거의 아무것도 바꾸지 않으면 더 늦은 체크포인트를 쓰고, 스타일과 가사를 확인하고, 곡을 늘리세요.',
        '말이 음악에서 어긋나면 가사를 확인하세요. 녹음에 없는 줄이나 반복이 타이밍을 흐트러뜨립니다.',
        '분리기 오류로 학습이 시작되지 않으면 보컬 분리기를 설치하거나 가사 타이밍을 끄세요.',
      ],
    },
    {
      title: '시작 전 체크리스트',
      checklist: [
        '같은 아티스트나 스타일의 곡 5~20곡, 좋은 품질.',
        '중복이나 잘린 조각이 없음.',
        '모든 곡에 BPM이 들어간 영어 한 문장 스타일, 아티스트 이름과 제목 없음.',
        '가사를 녹음과 대조하고 [Verse] / [Chorus]를 표시함.',
        '연주곡은 체크, 보컬 곡은 체크 해제.',
        '노란 삼각형 없음.',
        '드문 트리거 단어를 정함.',
        'VRAM 약 11 GB 여유, 생성은 멈춤.',
      ],
    },
  ],
};

export const trainingGuide: Record<Language, Guide> = { en, ru, zh, ja, ko };
