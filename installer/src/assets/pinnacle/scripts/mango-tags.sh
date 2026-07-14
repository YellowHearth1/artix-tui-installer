#!/bin/bash

# Динамічний індикатор тегів для MangoWC через mmsg IPC

# Отримуємо поточний тег через mmsg
current_tag=$(mmsg get_current_tag 2>/dev/null || echo "1")

# Створюємо HTML з підсвічуванням активного тегу
tags=""
for i in {1..9}; do
    if [ "$i" -eq "$current_tag" ]; then
        # Активний тег
        tags+="<span foreground='#33ccffee'><b>$i</b></span> "
    else
        # Неактивний тег
        tags+="<span foreground='#595959aa'>$i</span> "
    fi
done

# Виводимо JSON для waybar
echo "{\"text\": \"$tags\", \"tooltip\": \"Tag $current_tag\", \"class\": \"tags\"}"
