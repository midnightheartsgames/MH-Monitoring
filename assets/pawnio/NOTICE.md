# PawnIO — модули AMDFamily17 и IntelMSR

Температура и мощность CPU читаются через драйвер PawnIO (решение D4, PLAN.md §2.14): у AMD —
модулем `AMDFamily17`, у Intel — модулем `IntelMSR`. Сам драйвер ставит его официальный
установщик (<https://pawnio.eu>); приложение распространяет только модули, которые загружает в
драйвер.

## Что вложено

Оба файла взяты из одного архива `release_0_2_11.zip`:
<https://github.com/namazso/PawnIO.Modules/releases/tag/0.2.11>.

| Файл | Размер | SHA-256 | Процессоры | Используемые функции |
|---|---|---|---|---|
| `AMDFamily17.bin` | 10 652 байт | `DAE74615761B78BDF064DFB3E136252DDCC6FC727D88F14738D0E5800D427A91` | AMD 17h–1Ah (Zen 1–5) | `ioctl_read_msr`, `ioctl_read_smn` |
| `IntelMSR.bin` | 5 324 байт | `D6ED85D65AB17A22F813EF98207D6D537155EE2DED5976A21CB48413C9B92E5F` | Intel с цифровым датчиком | `ioctl_read_msr` |

| | |
|---|---|
| Версия | 0.2.11 |
| Лицензия | LGPL-2.1-or-later — полный текст в `COPYING` рядом |
| Автор | namazso &lt;admin@namazso.eu&gt; |
| Исходники | `AMDFamily17.p`, `IntelMSR.p` в том же репозитории, тег `0.2.11` |

`IntelMSR` разрешает читать только перечисленные в нём регистры. Приложение читает
`IA32_TEMPERATURE_TARGET`, `IA32_PACKAGE_THERM_STATUS`, `IA32_THERM_STATUS`, `RAPL_POWER_UNIT` и
`PKG_ENERGY_STATUS` и ничего не пишет.

Модули вложены без изменений: драйвер выполняет только подписанные модули, и любая правка сделала
бы их непригодными.

## Как приложение связано с PawnIO

Драйвер PawnIO распространяется под GPL-2.0 с исключением для программ, которые общаются с ним
только через интерфейс IOCTL устройства. Приложение именно так и работает: вызывает
`DeviceIoControl` напрямую и **не линкуется** с `PawnIOLib.dll`, лежащей в GPL-репозитории
драйвера. Модули под LGPL загружаются в драйвер как данные и с кодом приложения не связываются.

Проверить файлы:

```powershell
Get-FileHash .\AMDFamily17.bin, .\IntelMSR.bin -Algorithm SHA256
```
