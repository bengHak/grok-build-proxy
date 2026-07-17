package monitor

type selectionGroup uint8

const (
	selectionNone selectionGroup = iota
	selectionSessions
	selectionActive
	selectionRecent
	selectionErrors
)

type selectedItem struct {
	kind    string
	group   selectionGroup
	session *Session
	request *Request
}

func (v *View) rememberDashboardSelection(snapshot Snapshot) {
	item := selected(snapshot, v.Selection)
	v.selectionIndex = v.Selection
	v.selectionGroup = item.group
	v.selectionID = ""
	if item.session != nil {
		v.selectionID = item.session.identity()
	} else if item.request != nil {
		v.selectionID = item.request.identity()
	}
}

func (v *View) restoreDashboardSelection(snapshot Snapshot) {
	if v.Selection != v.selectionIndex {
		v.rememberDashboardSelection(snapshot)
		return
	}
	if v.selectionID == "" {
		return
	}
	item, index := selectedByID(snapshot, v.selectionID, v.selectionGroup)
	if index < 0 {
		return
	}
	v.Selection = index
	v.selectionGroup = item.group
}

func selected(snapshot Snapshot, target int) selectedItem {
	index := 0
	for i := range snapshot.Sessions {
		if index == target {
			return selectedItem{kind: "Session detail", group: selectionSessions, session: &snapshot.Sessions[i]}
		}
		index++
	}
	for groupIndex, group := range [][]Request{snapshot.Active, snapshot.Recent, snapshot.Errors} {
		for i := range group {
			if index == target {
				return selectedItem{kind: "Request detail", group: selectionGroup(int(selectionActive) + groupIndex), request: &group[i]}
			}
			index++
		}
	}
	return selectedItem{kind: "Detail"}
}

func selectedByID(snapshot Snapshot, id string, targetGroup selectionGroup) (selectedItem, int) {
	if targetGroup == selectionSessions {
		for i := range snapshot.Sessions {
			if snapshot.Sessions[i].identity() == id {
				return selectedItem{kind: "Session detail", group: selectionSessions, session: &snapshot.Sessions[i]}, i
			}
		}
		return selectedItem{kind: "Detail"}, -1
	}
	groups := [][]Request{snapshot.Active, snapshot.Recent, snapshot.Errors}
	groupIndex := int(targetGroup) - int(selectionActive)
	if groupIndex < 0 || groupIndex >= len(groups) {
		return selectedItem{kind: "Detail"}, -1
	}
	index := len(snapshot.Sessions)
	for i := 0; i < groupIndex; i++ {
		index += len(groups[i])
	}
	find := func(target int, base int) (selectedItem, int, bool) {
		for i := range groups[target] {
			if groups[target][i].identity() == id {
				group := selectionGroup(int(selectionActive) + target)
				return selectedItem{kind: "Request detail", group: group, request: &groups[target][i]}, base + i, true
			}
		}
		return selectedItem{}, -1, false
	}
	if item, foundIndex, ok := find(groupIndex, index); ok {
		return item, foundIndex
	}
	if targetGroup == selectionActive {
		recentIndex := len(snapshot.Sessions) + len(snapshot.Active)
		if item, foundIndex, ok := find(int(selectionRecent)-int(selectionActive), recentIndex); ok {
			return item, foundIndex
		}
	}
	return selectedItem{kind: "Detail"}, -1
}

func visibleRange(length, limit, selected int) (int, int) {
	if length == 0 || limit <= 0 {
		return 0, 0
	}
	start := 0
	if selected >= limit {
		start = selected - limit + 1
	}
	if start+limit > length {
		start = length - limit
		if start < 0 {
			start = 0
		}
	}
	end := start + limit
	if end > length {
		end = length
	}
	return start, end
}

func selectableCount(snapshot Snapshot) int {
	return len(snapshot.Sessions) + len(snapshot.Active) + len(snapshot.Recent) + len(snapshot.Errors)
}

func dashboardRowLimits(snapshot Snapshot, selection, budget int) [4]int {
	counts := [4]int{len(snapshot.Sessions), len(snapshot.Active), len(snapshot.Recent), len(snapshot.Errors)}
	selectedGroup := -1
	offset := 0
	for i, count := range counts {
		if selection >= offset && selection < offset+count {
			selectedGroup = i
			break
		}
		offset += count
	}

	var limits [4]int
	if selectedGroup >= 0 && budget > 0 {
		limits[selectedGroup] = 1
		budget--
	}
	for i, count := range counts {
		if budget == 0 {
			return limits
		}
		if i != selectedGroup && count > 0 {
			limits[i] = 1
			budget--
		}
	}
	for budget > 0 {
		added := false
		for i, count := range counts {
			if limits[i] >= count {
				continue
			}
			limits[i]++
			budget--
			added = true
			if budget == 0 {
				break
			}
		}
		if !added {
			break
		}
	}
	return limits
}
